// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's one Ed25519 verification: astream's strict check
//! ([`astream_aead::signature::verify`]). ring alone takes a key of small order, the identity
//! among them, whose one signature verifies every message; the strict check refuses such a key
//! and such a commitment, so a signature binds its key to what it signed.

/// Whether `signature` is `key`'s Ed25519 signature of `message`, strictly. A key that is not 32
/// bytes or a signature that is not 64 verifies nothing, as under ring.
pub(crate) fn verifies(key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    match (<&[u8; 32]>::try_from(key), <&[u8; 64]>::try_from(signature)) {
        (Ok(key), Ok(signature)) => astream_aead::signature::verify(key, message, signature),
        _ => false,
    }
}

/// Reuse an exact successful envelope signature fact. Record decoding uses [`verifies`]
/// directly: retaining another copy of each record's payload would violate its memory budget.
pub(crate) fn verifies_cached(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    static VERIFIED: Verified = Verified::new();
    VERIFIED.check(key, message, signature)
}

// A signature fact is independent of the current pin, roster, epoch and policy. Retain only
// successful strict checks, under the exact key, signature AND message bytes; callers still
// judge their current authority on every read. In particular a cached signature is no grant.
const VERIFIED_BYTES: usize = 4 * 1024 * 1024;
const VERIFIED_ENTRIES: usize = 4096;

struct Fact {
    key: [u8; 32],
    signature: [u8; 64],
    message: zeroize::Zeroizing<Vec<u8>>,
}

struct Verified(std::sync::Mutex<std::collections::VecDeque<Fact>>);

impl Verified {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(std::collections::VecDeque::new()))
    }

    fn check(&self, key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        self.check_with(key, message, signature, || {
            astream_aead::signature::verify(key, message, signature)
        })
    }

    fn check_with(
        &self,
        key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
        verify: impl FnOnce() -> bool,
    ) -> bool {
        if let Ok(facts) = self.0.lock()
            && facts.iter().any(|fact| {
                fact.key == *key
                    && fact.signature == *signature
                    && fact.message.as_slice() == message
            })
        {
            return true;
        }
        // Do not serialize cryptography or admit an unsuccessful check to the memo.
        if !verify() {
            return false;
        }
        if message.len() <= VERIFIED_BYTES
            && let Ok(mut facts) = self.0.lock()
        {
            let mut used: usize = facts.iter().map(|fact| fact.message.len()).sum();
            while (facts.len() >= VERIFIED_ENTRIES || used > VERIFIED_BYTES - message.len())
                && let Some(oldest) = facts.pop_front()
            {
                used -= oldest.message.len();
            }
            facts.push_back(Fact {
                key: *key,
                signature: *signature,
                message: zeroize::Zeroizing::new(message.to_vec()),
            });
        }
        true
    }
}

/// The identity point `01 00..00` as a key: a key of small order.
#[cfg(test)]
pub(crate) const SMALL_ORDER_KEY: [u8; 32] = {
    let mut key = [0; 32];
    key[0] = 1;
    key
};

/// The identity as commitment and `s = 0`: under ring alone, [`SMALL_ORDER_KEY`]'s signature of
/// every message (astream-aead's `a_small_order_key_signs_nothing`).
#[cfg(test)]
pub(crate) const FORGED: [u8; 64] = {
    let mut signature = [0; 64];
    signature[0] = 1;
    signature
};

/// The zero key `00..00`: the point `(√-1, 0)`, of order 4, which the record's shape and the
/// Node proof check once refused by name.
#[cfg(test)]
pub(crate) const ZERO_KEY: [u8; 32] = [0; 32];

/// The four points of [`ZERO_KEY`]'s subgroup, encoded: the identity, the key itself, its
/// negation and its double `(0, -1)`. With `s = 0`, ring alone takes a signature by the zero key
/// whose commitment is `-[k]A`, so one of these four signs a message when `k mod 4` matches it,
/// about one message in four for each.
#[cfg(test)]
const ZERO_KEY_SUBGROUP: [[u8; 32]; 4] = {
    let identity = SMALL_ORDER_KEY;
    let negated = {
        let mut point = [0; 32];
        point[31] = 0x80;
        point
    };
    let doubled = {
        let mut point = [0xff; 32];
        point[0] = 0xec;
        point[31] = 0x7f;
        point
    };
    [identity, ZERO_KEY, negated, doubled]
};

/// Whether ring's lax check alone takes `signature` as `key`'s of `message`: a test's proof that
/// the forgery the strict check refuses is one the replaced verifier took.
#[cfg(test)]
pub(crate) fn ring_takes(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's non-vacuity control: the lax check the strict one replaced"
    )]
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key);
    key.verify(message, signature).is_ok()
}

/// A signature by [`ZERO_KEY`] that ring alone takes over `message` (a commitment in the key's
/// subgroup and `s = 0`), if one of the four is.
#[cfg(test)]
pub(crate) fn zero_key_forgery(message: &[u8]) -> Option<[u8; 64]> {
    ZERO_KEY_SUBGROUP
        .iter()
        .map(|commitment| {
            let mut signature = [0; 64];
            signature[..32].copy_from_slice(commitment);
            signature
        })
        .find(|signature| ring_takes(&ZERO_KEY, message, signature))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_fact_requires_the_exact_key_message_and_signature() {
        let seed = [91; 32];
        let key = astream_aead::signature::public_key(&seed);
        let message = b"synthetic signed log record";
        let signature = astream_aead::signature::sign(&seed, message);
        let memo = Verified::new();
        let checks = std::cell::Cell::new(0);
        let check = |key: &[u8; 32], message: &[u8], signature: &[u8; 64]| {
            memo.check_with(key, message, signature, || {
                checks.set(checks.get() + 1);
                astream_aead::signature::verify(key, message, signature)
            })
        };
        assert!(check(&key, message, &signature));
        assert!(check(&key, message, &signature));
        assert_eq!(checks.get(), 1);
        // Negative controls warm the fact first: omitting any one part of its identity
        // would now accept a tuple the strict verifier refuses.
        assert!(!check(&key, b"changed signed log record", &signature));
        assert!(!check(&[0; 32], message, &signature));
        let mut changed = signature;
        changed[63] ^= 1;
        assert!(!check(&key, message, &changed));
        assert!(!check(&key, message, &changed));
        assert_eq!(checks.get(), 5, "refusals must not be retained");
        assert_eq!(memo.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn signature_facts_are_bounded_even_for_empty_messages() {
        let memo = Verified::new();
        for n in 0..=VERIFIED_ENTRIES {
            let mut seed = [0; 32];
            seed[..size_of::<usize>()].copy_from_slice(&n.to_le_bytes());
            let key = astream_aead::signature::public_key(&seed);
            let signature = astream_aead::signature::sign(&seed, b"");
            assert!(memo.check(&key, b"", &signature));
        }
        assert_eq!(memo.0.lock().unwrap().len(), VERIFIED_ENTRIES);
        let large = vec![42; VERIFIED_BYTES + 1];
        let key = astream_aead::signature::public_key(&[91; 32]);
        let signature = astream_aead::signature::sign(&[91; 32], &large);
        assert!(memo.check(&key, &large, &signature));
        let facts = memo.0.lock().unwrap();
        assert_eq!(facts.len(), VERIFIED_ENTRIES);
        assert!(facts.iter().all(|fact| fact.message.is_empty()));
    }

    #[test]
    fn a_byte_budget_eviction_requires_a_new_strict_check() {
        let memo = Verified::new();
        let seed = [91; 32];
        let key = astream_aead::signature::public_key(&seed);
        let first = vec![41; VERIFIED_BYTES / 2 + 1];
        let second = vec![42; first.len()];
        let a = astream_aead::signature::sign(&seed, &first);
        let b = astream_aead::signature::sign(&seed, &second);
        assert!(memo.check(&key, &first, &a));
        assert!(memo.check(&key, &second, &b));
        assert_eq!(memo.0.lock().unwrap().len(), 1);
        let checked = std::cell::Cell::new(false);
        assert!(memo.check_with(&key, &first, &a, || {
            checked.set(true);
            astream_aead::signature::verify(&key, &first, &a)
        }));
        assert!(checked.get());
    }
}
