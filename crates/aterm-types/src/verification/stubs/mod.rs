// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani-friendly replacement for `HashMap`.
//!
//! ONE CONSUMER, AND IT IS INVISIBLE TO AN ORDINARY BUILD. `aterm-vi`'s
//! `ViMarksMap` is this type under `cfg(kani)` and a `HashMap` otherwise, so no
//! normal compile — and no `pub(crate)` flip — sees the calls `ViMarks` makes.
//! Its whole surface is `new`/`insert`/`get`/`remove`/`contains_key`/`clear`
//! (plus `Clone`/`Default`/`Debug` for the struct's derives), and
//! `aterm-vi`'s `kani_marks_map_carries_the_marks_surface` test drives exactly
//! that set in the ordinary build. A dead-code sweep deleted four of those
//! methods on 2026-09-25 and broke the Kani build of `aterm-vi`; the test is
//! what keeps that from recurring.

use std::borrow::Borrow;
use std::collections::BTreeMap;

/// A verification-friendly map that uses `BTreeMap` internally.
///
/// This type provides a `HashMap`-like API but uses a tree structure
/// that Kani can verify efficiently without loop explosion (and without
/// `HashMap`'s `RandomState`, whose seeding FFI Kani cannot model).
#[derive(Debug)]
pub struct VerifyMap<K: Ord, V> {
    inner: BTreeMap<K, V>,
}

impl<K: Ord + Clone, V: Clone> Clone for VerifyMap<K, V> {
    // Hand-written ONLY so the skip can attach (identical to the derive
    // expansion). Skip: cloning the map dispatches into the INSTANTIATING
    // key/value `Clone` impls (open-trait user code). Verify-only.
    #[cfg_attr(trust_verify, trust::skip)]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K: Ord, V> VerifyMap<K, V> {
    /// Creates an empty map.
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    /// Inserts a key-value pair into the map.
    // Skip: `BTreeMap::insert` navigates via the CALLER-CHOSEN `K: Ord` (the
    // keyed-write per-corpus class) and allocates. Stub-tier, unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.inner.insert(key, value)
    }

    /// Removes a key from the map.
    // Skip: `K: Borrow<Q>` + `BTreeMap::remove` navigate via CALLER-CHOSEN
    // `Ord`/`Borrow` (the user-T keyed class). Stub-tier, unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.remove(key)
    }

    /// Returns a reference to the value for the key.
    // Skip: `K: Borrow<Q>` + `BTreeMap::get` navigate via CALLER-CHOSEN
    // `Ord`/`Borrow` (user-T keyed class). Stub-tier, unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.get(key)
    }

    /// Returns true if the map contains the key.
    // Skip: same caller-chosen Borrow/Ord keyed class as `get`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.inner.contains_key(key)
    }

    /// Clears the map.
    // Skip: clear runs each element's drop glue (drop-glue lane). Stub-tier.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

impl<K: Ord, V> Default for VerifyMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

// NOTE: `VerifyMap` deliberately does NOT implement `std::ops::Index`.
// Map indexing panics on an absent key by design (a reachable panic the Trust
// gate cannot discharge — key-presence is not modeled), and no caller in the
// workspace indexes a `VerifyMap`; callers use `get()` and handle `None`.
