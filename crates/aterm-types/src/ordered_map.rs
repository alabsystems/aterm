// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Insertion-ordered map and set backed by `Vec` + `HashMap`.
//!
//! Drop-in replacement for `indexmap::IndexMap` / `indexmap::IndexSet` for the
//! API surface used within the aterm workspace. Eliminates the `indexmap`
//! external dependency.
//!
//! All iteration yields elements in insertion order.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

// ---------------------------------------------------------------------------
// OrderedMap<K, V, S>
// ---------------------------------------------------------------------------

/// Insertion-ordered map backed by a `Vec<(K, V)>` and a `HashMap<K, usize>`.
///
/// Generic over hasher `S` so callers can substitute `FxBuildHasher` under Kani.
pub struct OrderedMap<K, V, S = std::hash::RandomState> {
    /// Entries in insertion order.
    entries: Vec<(K, V)>,
    /// Key -> index into `entries`.
    index: HashMap<K, usize, S>,
}

impl<K, V> OrderedMap<K, V, std::hash::RandomState>
where
    K: Eq + Hash + Clone,
{
    /// Create an empty map with the default hasher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<K, V, S> Default for OrderedMap<K, V, S>
where
    K: Eq + Hash + Clone,
    S: BuildHasher + Default,
{
    // Skip: `S::default()` is CALLER-CHOSEN code (the hasher type param —
    // open-trait user-T dispatch), and the HashMap ctor allocates.
    #[cfg_attr(trust_verify, trust::skip)]
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::with_hasher(S::default()),
        }
    }
}

impl<K, V, S> OrderedMap<K, V, S>
where
    K: Eq + Hash + Clone,
    S: BuildHasher,
{
    // Map invariant: every index stored in `self.index` is `< self.entries.len()`
    // and points at the entry with the matching key. All mutating methods
    // (`insert`, `shift_remove_full`, `move_index`, `split_off`, `drain`)
    // re-establish it before returning, so lookups below never see a stale
    // out-of-bounds index. The accessors use total `.get()`/`.get_mut()` access
    // with a documented dead `else`/`None` branch — rather than the former
    // panicking `entries[idx]` — which is behavior-identical on every real map
    // while discharging the Trust L0 bounds-check obligation (the same "no-op
    // under the documented invariant" discharge as `aterm-spec`'s
    // `as_int`/`as_bool`).

    /// Insert a key-value pair. If the key already exists, its value is updated
    /// in-place (preserving its position in the insertion order).
    pub fn insert(&mut self, key: K, value: V) {
        if let Some(&idx) = self.index.get(&key) {
            if let Some(entry) = self.entries.get_mut(idx) {
                entry.1 = value;
            }
            // else: unreachable under the map invariant (`idx < entries.len()`);
            // see the invariant comment above.
        } else {
            let idx = self.entries.len();
            self.index.insert(key.clone(), idx);
            self.entries.push((key, value));
        }
    }

    /// Returns `true` if the map contains `key`.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.index.contains_key(key)
    }
}

// ---------------------------------------------------------------------------
// IntoIterator
// ---------------------------------------------------------------------------

impl<K, V, S> IntoIterator for OrderedMap<K, V, S> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    // Skip: consumes the entry Vec into std's IntoIter (absent std body);
    // element drops run caller-chosen `K`/`V` glue (user-T dispatch).
    #[cfg_attr(trust_verify, trust::skip)]
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K, V> FromIterator<(K, V)> for OrderedMap<K, V>
where
    K: Eq + Hash + Clone,
{
    // Skip: `I: IntoIterator` is CALLER-CHOSEN code and the insert walk
    // hashes via caller-chosen `K: Hash + Eq` (user-T dispatch).
    #[cfg_attr(trust_verify, trust::skip)]
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

// ---------------------------------------------------------------------------
// OrderedSet<T, S>
// ---------------------------------------------------------------------------

/// Insertion-ordered set backed by [`OrderedMap<T, ()>`].
///
/// Drop-in replacement for `indexmap::IndexSet`.
pub struct OrderedSet<T, S = std::hash::RandomState> {
    inner: OrderedMap<T, (), S>,
}

impl<T> OrderedSet<T, std::hash::RandomState>
where
    T: Eq + Hash + Clone,
{
    /// Create an empty set with at least the specified capacity.
    ///
    /// The pre-allocation is capped at 2^20 entries; the set still grows on
    /// demand past the cap, so behavior is unchanged — the cap only bounds
    /// the up-front allocation (Trust L0: no unbounded `with_capacity` from
    /// an unconstrained argument). aterm's ordered collections are small, so
    /// the cap is never reached in practice.
    ///
    /// The bound is a literal (not a named const): the in-process verifier
    /// havocs named-const operands, which turns the clamp into an
    /// unconstrained symbol and refutes the allocation bound.
    #[must_use]
    // Skip: the HashMap/Vec reservation allocates and the hasher is
    // caller-chosen (`S: BuildHasher + Default` — user-T dispatch).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn with_capacity(capacity: usize) -> Self {
        // Clamp the up-front reservation with `.min(<literal>)` so the operand
        // reaching `with_capacity` carries a provable upper bound for the Trust
        // gate (the collection still grows on demand past the cap, so behaviour
        // is unchanged; aterm's ordered collections never reach it in practice).
        let capacity = capacity.min(1_048_576);
        Self {
            inner: OrderedMap {
                entries: Vec::with_capacity(capacity),
                index: HashMap::with_capacity(capacity),
            },
        }
    }
}

impl<T, S> Default for OrderedSet<T, S>
where
    T: Eq + Hash + Clone,
    S: BuildHasher + Default,
{
    fn default() -> Self {
        Self {
            inner: OrderedMap::default(),
        }
    }
}

impl<T, S> OrderedSet<T, S>
where
    T: Eq + Hash + Clone,
    S: BuildHasher,
{
    /// Insert a value. Returns `true` if the value was newly inserted.
    pub fn insert(&mut self, value: T) -> bool {
        if self.inner.contains_key(&value) {
            false
        } else {
            self.inner.insert(value, ());
            true
        }
    }
}

impl<'a, T, S> IntoIterator for &'a OrderedSet<T, S>
where
    T: Eq + Hash + Clone,
    S: BuildHasher,
{
    type Item = &'a T;
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (T, ())>, fn(&'a (T, ())) -> &'a T>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.entries.iter().map(|(k, _)| k)
    }
}

impl<T> FromIterator<T> for OrderedSet<T>
where
    T: Eq + Hash + Clone,
{
    // Skip: `I: IntoIterator` is CALLER-CHOSEN code and the insert walk hashes
    // via caller-chosen `T: Hash + Eq` (user-T dispatch).
    #[cfg_attr(trust_verify, trust::skip)]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let (lower, _) = iter.size_hint();
        // Clamp the size-hint at the call site too: this generic `from_iter`
        // is not monomorphized when the gate analyses it, so the callee's own
        // clamp is not visible here — bound the reservation with a literal so
        // there is no unbounded-allocation obligation (identity in practice).
        let mut set = Self::with_capacity(lower.min(1_048_576));
        for item in iter {
            set.insert(item);
        }
        set
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ===== OrderedMap =====

    #[test]
    fn map_contains_key() {
        let mut m = OrderedMap::new();
        m.insert(42u32, "hello");
        assert!(m.contains_key(&42));
        assert!(!m.contains_key(&99));
    }

    #[test]
    fn map_into_iter() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        let v: Vec<_> = m.into_iter().collect();
        assert_eq!(v, vec![("a", 1), ("b", 2)]);
    }

    // ===== OrderedSet =====
}
