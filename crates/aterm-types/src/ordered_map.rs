// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Insertion-ordered map and set backed by `Vec` + `HashMap`.
//!
//! Drop-in replacement for `indexmap::IndexMap` / `indexmap::IndexSet` for the
//! API surface used within the aterm workspace. Eliminates the `indexmap`
//! external dependency.
//!
//! All iteration yields elements in insertion order. Removal via
//! [`OrderedMap::shift_remove`] preserves order by shifting subsequent entries
//! (O(n)), which is acceptable for the small collections used in aterm.

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
    /// Rebuild index entries from `start` to end of `entries`.
    ///
    /// Used after operations that shift entry positions (remove, drain_front).
    fn fixup_indices(index: &mut HashMap<K, usize, S>, entries: &[(K, V)], start: usize) {
        for (i, (key, _)) in entries.iter().enumerate().skip(start) {
            if let Some(pos) = index.get_mut(key) {
                *pos = i;
            }
        }
    }

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

    /// Get a reference to the value for `key`.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        // `and_then` never yields `None` for a present key: the map invariant
        // guarantees `idx < entries.len()` (see the invariant comment above).
        self.index
            .get(key)
            .and_then(|&idx| self.entries.get(idx))
            .map(|entry| &entry.1)
    }

    /// Get a mutable reference to the value for `key`.
    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        // `and_then` never yields `None` for a present key: the map invariant
        // guarantees `idx < entries.len()` (see the invariant comment above).
        self.index
            .get(key)
            .copied()
            .and_then(move |idx| self.entries.get_mut(idx))
            .map(|entry| &mut entry.1)
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

    /// Number of entries.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the map is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over `(&K, &V)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Iterate over keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(k, _)| k)
    }

    /// Iterate over values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    /// Remove a key-value pair, shifting subsequent entries to preserve order.
    ///
    /// Returns the removed value, or `None` if the key was not present.
    /// O(n) due to the shift.
    pub fn shift_remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.shift_remove_full(key).map(|(_, _, v)| v)
    }

    /// Remove a key-value pair, shifting subsequent entries to preserve order.
    ///
    /// Returns `(index, key, value)` of the removed entry, or `None`.
    // Skip: `Q`/`K: Borrow<Q> + Hash + Eq` are CALLER-CHOSEN code (user-T
    // dispatch) and the shift walks the entry Vec + rebuilds the index.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn shift_remove_full<Q>(&mut self, key: &Q) -> Option<(usize, K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let idx = self.index.remove(key)?;
        let (k, v) = self.entries.remove(idx);
        // Fix up indices for entries that shifted left.
        Self::fixup_indices(&mut self.index, &self.entries, idx);
        Some((idx, k, v))
    }

    /// Get the index of a key in insertion order.
    #[inline]
    pub fn get_index_of<Q>(&self, key: &Q) -> Option<usize>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.index.get(key).copied()
    }

    /// Move an entry from position `from` to position `to`.
    ///
    /// Entries between `from` and `to` are shifted to fill the gap.
    /// Out-of-bounds indices are a no-op (matching `indexmap` behavior).
    // Skip: the entry-Vec rotate + index rebuild — the guarded-index class
    // (the bounds hold by the caller contract but do not chain in the
    // verifier's model) over caller-chosen `K: Hash + Eq`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn move_index(&mut self, from: usize, to: usize) {
        if from >= self.entries.len() || to >= self.entries.len() || from == to {
            return;
        }
        let lo = from.min(to);
        let hi = from.max(to);
        // Rotate the affected range in place instead of `remove` + `insert`.
        // A rotate of the slice `[lo..=hi]` by one is exactly "extract the
        // entry at `from` and re-insert it at `to`" (entries between shift by
        // one to fill the gap; entries outside the range are untouched), so
        // the result is identical — but it is length-preserving, which lets
        // the Trust gate discharge the bounds obligations that the
        // resize-then-index form (`Vec::remove`/`Vec::insert`, then
        // `entries[i]`) left unprovable. The guard above establishes
        // `lo <= hi < entries.len()`, so the slice is in bounds and non-empty
        // (`from != to`), making `rotate_*(1)`'s `mid <= len` precondition
        // locally provable.
        if from < to {
            self.entries[lo..=hi].rotate_left(1);
        } else {
            self.entries[lo..=hi].rotate_right(1);
        }
        // Rebuild the affected index range. `fixup_indices(lo)` rewrites
        // every position from `lo` to the end; positions past `hi` did not
        // move, so those writes store the value already present — identical
        // observable behavior to rewriting only `lo..=hi`, with no indexed
        // `entries[i]` access for the gate to refute.
        Self::fixup_indices(&mut self.index, &self.entries, lo);
    }

    /// Split the map at position `n`. Returns a new map containing
    /// `entries[n..]` while `self` retains `entries[..n]`.
    ///
    /// This matches `IndexMap::split_off` semantics: `split_off(n)` returns
    /// the tail, self keeps the head.
    #[must_use]
    // Skip: the entry-Vec split + index rebuild — guarded-index class over
    // caller-chosen `K: Hash + Eq` (user-T dispatch).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn split_off(&mut self, n: usize) -> Self
    where
        S: Default,
    {
        if n >= self.entries.len() {
            return Self::default();
        }
        // The early return above guarantees `n < len`, so the `.min(len)`
        // clamp is identity — it exists to make `Vec::split_off`'s in-range
        // precondition (`n <= len`) locally provable for the Trust gate,
        // whose default mode does not model the guard across the call.
        let split_at = n.min(self.entries.len());
        let tail_entries = self.entries.split_off(split_at);
        // Rebuild self.index for the remaining head entries (they didn't move).
        // We just need to remove keys that are now in the tail.
        self.index.retain(|_, idx| *idx < n);

        // Build index for the tail.
        let mut tail_index = HashMap::with_hasher(S::default());
        for (i, (k, _)) in tail_entries.iter().enumerate() {
            tail_index.insert(k.clone(), i);
        }
        Self {
            entries: tail_entries,
            index: tail_index,
        }
    }

    /// Drain all entries from the map, yielding `(K, V)` pairs in order.
    // Skip: `Vec::drain` is BLANKET-unmodeled (resize-aware length tracking
    // pending) and the index rebuild walks caller-chosen `K: Hash + Eq`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn drain(&mut self, _range: std::ops::RangeFull) -> std::vec::Drain<'_, (K, V)> {
        self.index.clear();
        self.entries.drain(..)
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
    /// Create an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: OrderedMap::new(),
        }
    }

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

    /// Returns `true` if the set contains `value`.
    #[inline]
    pub fn contains<Q>(&self, value: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.inner.contains_key(value)
    }

    /// Number of elements.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if the set is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Drain the first `n` elements from the set, preserving order.
    ///
    /// This matches the `IndexSet::drain(..n)` pattern used for FIFO eviction.
    // Skip: `Vec::drain` is BLANKET-unmodeled (resize-aware length tracking
    // pending) and the index rebuild hashes via caller-chosen `T: Hash + Eq`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn drain_front(&mut self, n: usize) {
        let n = n.min(self.inner.entries.len());
        if n == 0 {
            return;
        }
        // Remove index entries for the keys about to be drained.
        // `take(n)` iterates the same clamped prefix as `[..n]` without a
        // slice bounds obligation (Trust L0).
        for (key, _) in self.inner.entries.iter().take(n) {
            self.inner.index.remove(key);
        }
        // Discard the front entries.
        self.inner.entries.drain(..n);
        // Fix up indices: everything shifted left by `n`.
        OrderedMap::<T, (), S>::fixup_indices(&mut self.inner.index, &self.inner.entries, 0);
    }

    /// Iterate over values in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.inner.entries.iter().map(|(k, _)| k)
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
    fn map_insert_and_get() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
        assert_eq!(m.get("c"), None);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn map_insert_updates_in_place() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("a", 10);
        assert_eq!(m.get("a"), Some(&10));
        assert_eq!(m.len(), 2);
        // Order preserved: a still before b.
        let keys: Vec<_> = m.keys().copied().collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn map_shift_remove() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);
        assert_eq!(m.shift_remove("b"), Some(2));
        assert_eq!(m.len(), 2);
        let keys: Vec<_> = m.keys().copied().collect();
        assert_eq!(keys, vec!["a", "c"]);
        // Index consistency check.
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("c"), Some(&3));
        assert_eq!(m.get("b"), None);
    }

    #[test]
    fn map_shift_remove_full() {
        let mut m = OrderedMap::new();
        m.insert("x", 10);
        m.insert("y", 20);
        m.insert("z", 30);
        let result = m.shift_remove_full("y");
        assert_eq!(result, Some((1, "y", 20)));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn map_move_index() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);
        // Move "a" (index 0) to back (index 2).
        m.move_index(0, 2);
        let keys: Vec<_> = m.keys().copied().collect();
        assert_eq!(keys, vec!["b", "c", "a"]);
        // Verify index is correct after move.
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
        assert_eq!(m.get("c"), Some(&3));
    }

    /// In release builds, out-of-bounds move_index is a silent no-op.
    /// Out-of-bounds move_index is a silent no-op.
    #[test]
    fn map_move_index_out_of_bounds_is_noop() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        // Both out-of-bounds cases should be no-ops in release.
        m.move_index(5, 0);
        m.move_index(0, 5);
        let keys: Vec<_> = m.keys().copied().collect();
        assert_eq!(keys, vec!["a", "b"]);
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
    }

    #[test]
    fn map_split_off() {
        let mut m = OrderedMap::<&str, i32>::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);
        m.insert("d", 4);
        let tail = m.split_off(2);
        let head_keys: Vec<_> = m.keys().copied().collect();
        let tail_keys: Vec<_> = tail.keys().copied().collect();
        assert_eq!(head_keys, vec!["a", "b"]);
        assert_eq!(tail_keys, vec!["c", "d"]);
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(tail.get("c"), Some(&3));
    }

    #[test]
    fn map_drain() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        let drained: Vec<_> = m.drain(..).collect();
        assert_eq!(drained, vec![("a", 1), ("b", 2)]);
        assert!(m.is_empty());
    }

    #[test]
    fn map_contains_key() {
        let mut m = OrderedMap::new();
        m.insert(42u32, "hello");
        assert!(m.contains_key(&42));
        assert!(!m.contains_key(&99));
    }

    #[test]
    fn map_get_index_of() {
        let mut m = OrderedMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);
        assert_eq!(m.get_index_of("a"), Some(0));
        assert_eq!(m.get_index_of("b"), Some(1));
        assert_eq!(m.get_index_of("c"), Some(2));
        assert_eq!(m.get_index_of("d"), None);
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

    #[test]
    fn set_insert_and_contains() {
        let mut s = OrderedSet::new();
        assert!(s.insert("a"));
        assert!(s.insert("b"));
        assert!(!s.insert("a")); // duplicate
        assert_eq!(s.len(), 2);
        assert!(s.contains("a"));
        assert!(s.contains("b"));
        assert!(!s.contains("c"));
    }

    #[test]
    fn set_drain_front() {
        let mut s = OrderedSet::new();
        for i in 0..10 {
            s.insert(i);
        }
        s.drain_front(3);
        assert_eq!(s.len(), 7);
        assert!(!s.contains(&0));
        assert!(!s.contains(&1));
        assert!(!s.contains(&2));
        assert!(s.contains(&3));
        assert!(s.contains(&9));
    }

    #[test]
    fn set_iter() {
        let mut s = OrderedSet::new();
        s.insert("x");
        s.insert("y");
        s.insert("z");
        let items: Vec<_> = s.iter().copied().collect();
        assert_eq!(items, vec!["x", "y", "z"]);
    }

    #[test]
    fn set_for_loop() {
        let mut s = OrderedSet::new();
        s.insert(1);
        s.insert(2);
        s.insert(3);
        let mut collected = Vec::new();
        for &val in &s {
            collected.push(val);
        }
        assert_eq!(collected, vec![1, 2, 3]);
    }
}
