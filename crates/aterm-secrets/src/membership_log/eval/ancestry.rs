// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact reachability over one immutable ordered history, with a fixed row payload budget.

use super::Bits;
use crate::membership_log::{Record, RecordId};
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

const ROW_BYTES: usize = 2 * 1024 * 1024;
const NONE: usize = usize::MAX;

macro_rules! count {
    ($cache:expr, $field:ident, $amount:expr) => {
        #[cfg(test)]
        {
            $cache.metrics.$field.fetch_add($amount, Ordering::Relaxed);
        }
    };
}

/// Derived data shared only by an identical ordered complete history.
#[derive(Debug)]
pub(in crate::membership_log) struct Ancestry {
    ids: Vec<RecordId>,
    pub(super) at: BTreeMap<RecordId, usize>,
    parents: Vec<Vec<usize>>,
    published: Option<Vec<OnceLock<Bits>>>,
    cache: Mutex<Cache>,
    #[cfg(test)]
    metrics: Arc<Metrics>,
}

impl Ancestry {
    pub(in crate::membership_log) fn new(records: &[Record]) -> Arc<Self> {
        Self::with_budget(records, ROW_BYTES)
            .expect("a bounded membership log fits one ancestry row")
    }

    pub(super) fn with_budget(
        records: &[Record],
        budget: usize,
    ) -> Result<Arc<Self>, &'static str> {
        let ids: Vec<_> = records.iter().map(|r| *r.id()).collect();
        let at: BTreeMap<_, _> = ids.iter().copied().zip(0..ids.len()).collect();
        let mut parents = Vec::with_capacity(records.len());
        for record in records {
            let mut row = Vec::with_capacity(record.parents().len());
            for id in record.parents() {
                row.push(*at.get(id).ok_or("ancestry parent is not held")?);
            }
            parents.push(row);
        }
        Self::from_parts(ids, at, parents, budget)
    }

    fn from_parts(
        ids: Vec<RecordId>,
        at: BTreeMap<RecordId, usize>,
        parents: Vec<Vec<usize>>,
        budget: usize,
    ) -> Result<Arc<Self>, &'static str> {
        let n = ids.len();
        if parents.len() != n || at.len() != n {
            return Err("ancestry history has repeated or missing records");
        }
        if parents
            .iter()
            .enumerate()
            .any(|(i, row)| row.iter().any(|&p| p >= i))
        {
            return Err("ancestry parents must precede their child");
        }
        let row_bytes = n
            .div_ceil(64)
            .checked_mul(8)
            .ok_or("ancestry row is too large")?;
        if row_bytes > budget {
            return Err("ancestry budget cannot hold one row");
        }
        let cache = Cache::new(n, budget);
        let published = (cache.capacity == n).then(|| (0..n).map(|_| OnceLock::new()).collect());
        #[cfg(test)]
        let metrics = Arc::clone(&cache.metrics);
        Ok(Arc::new(Self {
            ids,
            at,
            parents,
            published,
            cache: Mutex::new(cache),
            #[cfg(test)]
            metrics,
        }))
    }

    pub(super) fn matches(&self, records: &[Record]) -> bool {
        self.ids.len() == records.len()
            && self
                .ids
                .iter()
                .zip(records)
                .all(|(id, record)| id == record.id())
    }

    pub(super) fn before(self: &Arc<Self>, a: usize, b: usize) -> bool {
        self.session().before(a, b)
    }

    /// All-fit sessions borrow immutable published rows; only a missing row locks its builder.
    /// An evictable session owns the cache guard, and never invokes semantic caller code.
    pub(super) fn session(self: &Arc<Self>) -> Session<'_> {
        Session {
            owner: self,
            cache: self.published.is_none().then(|| {
                let mut cache = self
                    .cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                self.recover_cache(&mut cache);
                RefCell::new(cache)
            }),
        }
    }

    // Called only with this index's cache guard. Published rows outlive builder recovery.
    fn recover_cache(&self, cache: &mut Cache) {
        if !self.cache.is_poisoned() {
            return;
        }
        let budget = cache.budget;
        #[cfg(test)]
        let metrics = Arc::clone(&cache.metrics);
        *cache = Cache::new(self.ids.len(), budget);
        #[cfg(test)]
        {
            metrics.scratch_words.fetch_add(
                cache.metrics.scratch_words.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            cache.metrics = metrics;
        }
        if let Some(published) = &self.published {
            cache.occupied = published.iter().filter(|row| row.get().is_some()).count();
        }
        self.cache.clear_poison();
    }

    #[cfg(test)]
    pub(super) fn from_parents_for_tests(
        parents: Vec<Vec<usize>>,
        budget: usize,
    ) -> Result<Arc<Self>, &'static str> {
        let ids: Vec<RecordId> = (0..parents.len())
            .map(|i| {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&(i as u64).to_le_bytes());
                id
            })
            .collect();
        let at = ids.iter().copied().zip(0..ids.len()).collect();
        Self::from_parts(ids, at, parents, budget)
    }

    #[cfg(test)]
    pub(super) fn with_builder_held_for_test(&self, action: impl FnOnce()) {
        let mut guard = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.recover_cache(&mut guard);
        action();
    }
}

/// A single pure evaluation borrows one session; individual row borrows never escape.
pub(super) struct Session<'a> {
    owner: &'a Arc<Ancestry>,
    cache: Option<RefCell<MutexGuard<'a, Cache>>>,
}

impl Session<'_> {
    pub(super) fn owner(&self) -> Arc<Ancestry> {
        Arc::clone(self.owner)
    }

    pub(super) fn matches(&self, records: &[Record]) -> bool {
        self.owner.matches(records)
    }

    pub(super) fn at(&self) -> &BTreeMap<RecordId, usize> {
        &self.owner.at
    }

    pub(super) fn before(&self, a: usize, b: usize) -> bool {
        count!(self.owner, before_queries, 1);
        count!(
            self.owner,
            fast_rejects,
            usize::from(a >= b || b >= self.owner.ids.len())
        );
        a < b && b < self.owner.ids.len() && self.with_row(b, |row| row.get(a))
    }

    pub(super) fn upto(&self, a: usize, b: usize) -> bool {
        (a == b && b < self.owner.ids.len()) || self.before(a, b)
    }

    pub(super) fn union_past_into(&self, b: usize, into: &mut Bits) {
        self.with_row(b, |row| into.union(row));
        count!(
            self.owner,
            union_words,
            into.0.len().min(self.owner.ids.len().div_ceil(64))
        );
    }

    pub(super) fn copy_past_into(&self, b: usize, into: &mut Bits) {
        self.with_row(b, |row| into.0.copy_from_slice(&row.0));
        count!(self.owner, copy_words, into.0.len());
    }

    pub(super) fn past_intersects(&self, b: usize, set: &Bits) -> bool {
        let found = self.with_row(b, |row| {
            row.0.iter().zip(&set.0).position(|(a, b)| a & b != 0)
        });
        count!(
            self.owner,
            intersection_words,
            found.map_or(set.0.len().min(self.owner.ids.len().div_ceil(64)), |i| i
                + 1)
        );
        found.is_some()
    }

    fn with_row<T>(&self, b: usize, use_row: impl FnOnce(&Bits) -> T) -> T {
        count!(self.owner, requests, 1);
        if let Some(published) = &self.owner.published {
            if let Some(row) = published[b].get() {
                count!(self.owner, hits, 1);
                return use_row(row);
            }
            let mut cache = self
                .owner
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.owner.recover_cache(&mut cache);
            cache.ensure(b, &self.owner.parents, Some(published));
            drop(cache);
            return use_row(published[b].get().unwrap());
        }
        let mut cache = self.cache.as_ref().unwrap().borrow_mut();
        cache.ensure(b, &self.owner.parents, None);
        use_row(cache.rows[b].as_ref().unwrap())
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> Cost {
        self.owner.metrics.snapshot()
    }
}

#[cfg(test)]
macro_rules! metrics {
    ($($field:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub(super) struct Cost { $(pub(super) $field: usize,)+ }
        #[derive(Debug, Default)]
        struct Metrics { $($field: AtomicUsize,)+ }
        impl Metrics {
            fn snapshot(&self) -> Cost {
                Cost { $($field: self.$field.load(Ordering::Relaxed),)+ }
            }
        }
    };
}

// `requests` counts actual row requests; `before_queries` includes topological rejects.
#[cfg(test)]
metrics!(
    before_queries,
    fast_rejects,
    requests,
    hits,
    builds,
    evictions,
    edges,
    zero_words,
    union_words,
    copy_words,
    intersection_words,
    scratch_words,
    scan_words,
    scratch_word_bytes,
    scratch_ordinal_bytes,
    scratch_capacity_bytes,
    peak_row_bytes,
    peak_stack
);

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct Cache {
    len: usize,
    rows: Vec<Option<Bits>>,
    previous: Vec<usize>,
    next: Vec<usize>,
    first: usize,
    last: usize,
    occupied: usize,
    capacity: usize,
    budget: usize,
    row_bytes: usize,
    needed: Bits,
    queued: Bits,
    stack: Vec<usize>,
    order: Vec<usize>,
    #[cfg(test)]
    metrics: Arc<Metrics>,
}

impl Cache {
    fn new(n: usize, budget: usize) -> Self {
        let row_bytes = n.div_ceil(64) * 8;
        let capacity = budget.checked_div(row_bytes).unwrap_or(0).min(n);
        let evictable = capacity < n;
        let cache = Self {
            len: n,
            rows: if evictable { vec![None; n] } else { Vec::new() },
            previous: if evictable { vec![NONE; n] } else { Vec::new() },
            next: if evictable { vec![NONE; n] } else { Vec::new() },
            first: NONE,
            last: NONE,
            occupied: 0,
            capacity,
            budget,
            row_bytes,
            needed: Bits::new(n),
            queued: Bits::new(n),
            stack: Vec::with_capacity(n),
            order: Vec::with_capacity(n),
            #[cfg(test)]
            metrics: Arc::default(),
        };
        #[cfg(test)]
        {
            let words = (cache.needed.0.capacity() + cache.queued.0.capacity())
                * std::mem::size_of::<u64>();
            let ordinals =
                (cache.stack.capacity() + cache.order.capacity()) * std::mem::size_of::<usize>();
            cache
                .metrics
                .scratch_word_bytes
                .store(words, Ordering::Relaxed);
            cache
                .metrics
                .scratch_ordinal_bytes
                .store(ordinals, Ordering::Relaxed);
            cache
                .metrics
                .scratch_capacity_bytes
                .store(words + ordinals, Ordering::Relaxed);
            cache.metrics.scratch_words.store(
                cache.needed.0.len() + cache.queued.0.len(),
                Ordering::Relaxed,
            );
        }
        cache
    }

    fn ensure(&mut self, b: usize, parents: &[Vec<usize>], published: Option<&[OnceLock<Bits>]>) {
        if let Some(published) = published {
            if published[b].get().is_some() {
                count!(self, hits, 1);
            } else {
                self.fill_fitting(b, parents, published);
            }
            return;
        }
        if self.rows[b].is_some() {
            count!(self, hits, 1);
            self.touch(b);
            return;
        }
        {
            let mut row = self.row();
            self.queued.0.fill(0);
            count!(self, scratch_words, self.queued.0.len());
            self.stack.clear();
            for &p in &parents[b] {
                count!(self, edges, 1);
                self.push(p);
            }
            while let Some(p) = self.stack.pop() {
                if row.get(p) {
                    continue;
                }
                row.set(p);
                if let Some(known) = &self.rows[p] {
                    row.union(known);
                    count!(self, union_words, row.0.len());
                    self.touch(p);
                } else {
                    for &parent in &parents[p] {
                        count!(self, edges, 1);
                        self.push(parent);
                    }
                }
            }
            self.insert(b, row);
        }
    }

    fn fill_fitting(&mut self, b: usize, parents: &[Vec<usize>], published: &[OnceLock<Bits>]) {
        self.needed.0.fill(0);
        self.queued.0.fill(0);
        count!(
            self,
            scratch_words,
            self.needed.0.len() + self.queued.0.len()
        );
        self.stack.clear();
        self.order.clear();
        self.push(b);
        while let Some(i) = self.stack.pop() {
            if published[i].get().is_some() {
                continue;
            }
            self.needed.set(i);
            for &p in &parents[i] {
                count!(self, edges, 1);
                if published[p].get().is_none() {
                    self.push(p);
                }
            }
        }
        for (word, &bits) in self.needed.0.iter().enumerate() {
            count!(self, scan_words, 1);
            let mut bits = bits;
            while bits != 0 {
                self.order.push(word * 64 + bits.trailing_zeros() as usize);
                bits &= bits - 1;
            }
        }
        for at in 0..self.order.len() {
            let i = self.order[at];
            let mut row = self.row();
            for &p in &parents[i] {
                count!(self, edges, 1);
                row.union(published[p].get().unwrap());
                count!(self, union_words, row.0.len());
                row.set(p);
            }
            assert!(
                published[i].set(row).is_ok(),
                "one ancestry builder publishes each row"
            );
            self.occupied += 1;
            count!(self, builds, 1);
        }
    }

    fn push(&mut self, i: usize) {
        if !self.queued.get(i) {
            self.queued.set(i);
            self.stack.push(i);
            #[cfg(test)]
            {
                self.metrics
                    .peak_stack
                    .fetch_max(self.stack.len(), Ordering::Relaxed);
            }
        }
    }

    fn row(&mut self) -> Bits {
        while self.occupied == self.capacity {
            let last = self.last;
            assert_ne!(last, NONE, "one row must fit the ancestry cache");
            self.unlink(last);
            self.rows[last] = None;
            self.occupied -= 1;
            count!(self, evictions, 1);
        }
        assert!((self.occupied + 1) * self.row_bytes <= self.budget);
        count!(self, zero_words, self.row_bytes / 8);
        #[cfg(test)]
        {
            self.metrics
                .peak_row_bytes
                .fetch_max((self.occupied + 1) * self.row_bytes, Ordering::Relaxed);
        }
        Bits::new(self.len)
    }

    fn insert(&mut self, at: usize, row: Bits) {
        assert!(self.rows[at].is_none());
        self.rows[at] = Some(row);
        self.occupied += 1;
        count!(self, builds, 1);
        self.touch(at);
    }

    fn unlink(&mut self, at: usize) {
        let previous = self.previous[at];
        let next = self.next[at];
        if previous == NONE {
            self.first = next;
        } else {
            self.next[previous] = next;
        }
        if next == NONE {
            self.last = previous;
        } else {
            self.previous[next] = previous;
        }
        self.previous[at] = NONE;
        self.next[at] = NONE;
    }

    fn touch(&mut self, at: usize) {
        if self.first == at {
            return;
        }
        if self.previous[at] != NONE || self.next[at] != NONE {
            self.unlink(at);
        }
        self.next[at] = self.first;
        if self.first == NONE {
            self.last = at;
        } else {
            self.previous[self.first] = at;
        }
        self.first = at;
    }
}
