// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P5's aterm half, the relay (step PF5): which keys a Mac's brokers take as origins and which
//! they revoke, by key and by principal, from one read of the membership log (one-mesh §4.6,
//! "Removal follows the log"; `docs/design/one-mesh-proofs/P5-relay.md`).
//!
//! A broker files an origin, a revocation and a relayed record's dedup key by the key's 64-bit
//! principal, so two keys whose principals collide (or whose producer ids do) are one origin to
//! it, each able to shadow the other. So keys are judged by class, the producer id a broker files
//! a key under, and current wins: a current key alone in its class is an origin (this Mac's own
//! always wins its class); two or more current keys of one class are contested, none an origin;
//! a removed, retired or contested key is revoked by its key, and its principal is revoked unless
//! an origin holds that principal; this Mac's own key and principal are never revoked on its own
//! brokers.
//!
//! The decisions are the scalar `contract!` kernels below, each stating `ensures result ==
//! spec_<name>(..) by <name>_is_exact` for a definition of `relay_island.rs`. That island (every
//! Trust build) proves, for every number of keys, classes and principals and for every sequence of
//! reads: the brokers' state after each pass is the verdict of the read alone, whatever came
//! before (`standings_safe`); no origin stands revoked, by its key or its principal
//! (NoCurrentSeatRevoked); at most one origin per class; this Mac is its own origin; a gone or
//! contested key is revoked; the per-key principal lift before PF5 breaks the law; and a record
//! a neighbour answered unknown is delivered once it seats the origin, under fair probes.
//! `relay_binding_island.rs` proves each compiled kernel equal to its spec and closes the laws for
//! the World that asks the compiled kernels. [`Verdict::of`]'s folds are the island's, step for
//! step; until loops reach Trust's E6 import, the answer sheet `relay::tests` recomputes holds
//! them to the island (TG12 removes it).

use crate::revocation::any_n;

/// One key of a read, as dense ids the caller interns: the class a broker files it under
/// (`producer_id_of(identity_principal(key))`), its principal, whether the read seats it
/// (`Standings::is_current`, never with `gone`), and whether its standing is `Removed` or
/// `Retired`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub class: usize,
    pub principal: usize,
    pub current: bool,
    pub gone: bool,
}

contract! {
    /// A key is an origin: this Mac's own, or current with no rival in its class.
    pub fn origin(is_me: bool, current: bool, rival: bool) -> bool
    clauses [ensures result == spec_origin(is_me, current, rival) by origin_is_exact]
    { is_me || (current && !rival) }
}
contract! {
    /// A key is revoked by its key: not this Mac's, and gone, or current with a rival (contested).
    pub fn key_revoked(is_me: bool, current: bool, rival: bool, gone: bool) -> bool
    clauses [ensures result == spec_key_revoked(is_me, current, rival, gone) by key_revoked_is_exact]
    { !is_me && (gone || (current && rival)) }
}
contract! {
    /// A principal is revoked: some key of it is, and no origin holds it.
    pub fn principal_revoked(key_revoked: bool, origin_holds: bool) -> bool
    clauses [ensures result == spec_principal_revoked(key_revoked, origin_holds) by principal_revoked_is_exact]
    { key_revoked && !origin_holds }
}

#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "relay_island.rs"]
mod island;

/// What the brokers take of one read: per key, whether it has a rival, is an origin and is
/// revoked; per principal, whether it is revoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    rival: Vec<bool>,
    origin: Vec<bool>,
    revoked: Vec<bool>,
    principal_revoked: Vec<bool>,
}

impl Verdict {
    /// The verdict on `keys` for the Mac whose key is `keys[me]`, its principals `0..principals`.
    /// Each fold is the island's (`rival`, `originW`, `revokedW`, `revpW`, `holdsW`,
    /// `prinRevokedW`): a left fold over every key, no early exit.
    #[must_use]
    pub fn of(keys: &[Key], me: usize, principals: usize) -> Self {
        let n = keys.len();
        let rival: Vec<bool> = (0..n)
            .map(|k| {
                any_n(n, |j| {
                    j != k && ((keys[j].current || j == me) && keys[j].class == keys[k].class)
                })
            })
            .collect();
        let origin: Vec<bool> = (0..n)
            .map(|k| origin(k == me, keys[k].current, rival[k]))
            .collect();
        let revoked: Vec<bool> = (0..n)
            .map(|k| key_revoked(k == me, keys[k].current, rival[k], keys[k].gone))
            .collect();
        let principal_revoked = (0..principals)
            .map(|p| {
                principal_revoked(
                    any_n(n, |k| keys[k].principal == p && revoked[k]),
                    any_n(n, |k| keys[k].principal == p && origin[k]),
                )
            })
            .collect();
        Self {
            rival,
            origin,
            revoked,
            principal_revoked,
        }
    }

    /// Whether another current key, this Mac's own counted, shares key `k`'s class.
    #[must_use]
    pub fn has_rival(&self, k: usize) -> bool {
        self.rival[k]
    }

    /// Whether key `k` is an origin: admitted, a seat, and given to every broker.
    #[must_use]
    pub fn is_origin(&self, k: usize) -> bool {
        self.origin[k]
    }

    /// Whether key `k` is revoked by its key.
    #[must_use]
    pub fn is_revoked(&self, k: usize) -> bool {
        self.revoked[k]
    }

    /// Whether principal `p` is revoked.
    #[must_use]
    pub fn is_principal_revoked(&self, p: usize) -> bool {
        self.principal_revoked[p]
    }
}

#[cfg(test)]
mod tests;
