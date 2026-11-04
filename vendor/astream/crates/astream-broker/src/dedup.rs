// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The exactly-once ingest index: `(producer_id, producer_seq) -> Offset` for every retained
//! produce, exact, at a few bytes a record.
//!
//! The contract is the PRECISE pair (`store.rs`'s head): a producer may re-send an old seq
//! after a crash, so no per-producer high-water mark can stand in for it, and the index must
//! answer every question a `HashMap<(u64, u64), Offset>` answered before it: a retained key's
//! original offset, its absence once retention cut its record, the count of keys, and an
//! insert of a key already held replaces its offset (a replica that holds a key twice,
//! because its retention kept an old record its leader pruned and then re-took, answers with
//! the newer, as the map did).
//!
//! The map paid one hash entry a record (a 24-byte entry and its control byte, about 52 B a
//! record by its fill; `docs/DESIGN-aterm-fabric.md` §14 open question 8). Producers mostly
//! publish consecutive seqs, so a producer's keys are PACKED once two of them NEIGHBOUR (one
//! seq apart): it keeps every key from then on sorted by seq in chunks of at most `CHUNK`
//! keys, each chunk as maximal runs of consecutive seqs (`Run`: the run's first seq and where
//! its entries start, 16 B) and one offset per key as a `u32` from the chunk's base. A
//! producer that publishes `1, 2, 3, …` costs 4 B a record and its chunks' headers under a
//! tenth of a byte more; a sparse key beside its runs (an ack's `ACK_SEQ_BASE | input`, which
//! a processor publishes under the id it publishes with) is a run of one, 20 B. A producer
//! none of whose keys neighbour keeps each LOOSE, one hash entry as the map's was, so a
//! producer that publishes a key or two (`asb pub` with no `--id`) costs what the map cost
//! and no more. Every key lives in exactly one of the two, and no loose key has a neighbour
//! held: an insert beside a loose key packs both.
//!
//! WHAT IS EXACT, AND WHAT IS ONLY FAST. Exactness never depends on how seqs or offsets
//! arrive: every key is placed by its seq, a lookup searches its producer's chunks and runs and
//! then the loose keys, and a retention cut drops exactly the keys whose offsets lie below it. The chunk bounds
//! what an insert inside the order costs (a second stream under one id, such as a
//! processor's acks above its publishes, or a re-sent old seq): it moves at most a chunk's
//! offsets and runs, and a full chunk splits in halves. What depends on the broker's order
//! (every insert at the log's head, so above every offset held) is a cut's speed: then a
//! chunk's offsets increase in seq order (`Chunk::ordered`) and a cut drops a prefix of it;
//! any other chunk is filtered through at most a chunk's keys of scratch. A chunk whose
//! offsets span more than `u32::MAX` keeps them as `u64`s (`Offs::Wide`), so no span is
//! ever wrong.

use astream_wire::Offset;
use std::collections::HashMap;

/// The most keys a chunk holds: what an insert inside the order moves at most (its offsets
/// and its runs), and what a cut's filter of a chunk copies at most. A full chunk's header
/// and its run cost under a tenth of a byte a key.
const CHUNK: usize = 1024;

/// The chunk size in force: `CHUNK`, which a test may lower on its own thread so that a
/// short stream crosses chunk boundaries.
fn chunk_cap() -> usize {
    #[cfg(test)]
    {
        tally::chunk_cap()
    }
    #[cfg(not(test))]
    {
        CHUNK
    }
}

/// A maximal run of consecutive producer seqs: `seq0, seq0 + 1, …`, whose offsets are
/// `Chunk::offs[at..]` up to the next run's `at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    seq0: u64,
    at: usize,
}

/// One chunk's offsets, one per key, in seq order.
#[derive(Debug)]
enum Offs {
    /// Each offset as its distance from `base`, which is at or below every one.
    Narrow { base: u64, v: Vec<u32> },
    /// Offsets spanning more than `u32::MAX`, kept whole.
    Wide(Vec<u64>),
}

impl Offs {
    /// The `n` offsets `all` yields, narrow when their span fits a `u32`, at capacity `n`.
    fn of(all: impl Iterator<Item = u64> + Clone, n: usize) -> Offs {
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        for o in all.clone() {
            lo = lo.min(o);
            hi = hi.max(o);
        }
        if lo > hi {
            return Offs::Narrow {
                base: 0,
                v: Vec::new(),
            };
        }
        if hi - lo > u64::from(u32::MAX) {
            let mut v = Vec::with_capacity(n);
            v.extend(all);
            return Offs::Wide(v);
        }
        let mut v = Vec::with_capacity(n);
        for o in all {
            v.push(Offs::narrow(lo, o));
        }
        Offs::Narrow { base: lo, v }
    }

    fn len(&self) -> usize {
        match self {
            Offs::Narrow { v, .. } => v.len(),
            Offs::Wide(v) => v.len(),
        }
    }

    fn get(&self, j: usize) -> Option<u64> {
        match self {
            Offs::Narrow { base, v } => match v.get(j) {
                Some(&d) => base.checked_add(u64::from(d)),
                None => None,
            },
            Offs::Wide(v) => v.get(j).copied(),
        }
    }

    /// Every offset, in seq order.
    fn to_vec(&self) -> Vec<u64> {
        let mut out = Vec::with_capacity(self.len());
        for j in 0..self.len() {
            if let Some(o) = self.get(j) {
                out.push(o);
            }
        }
        out
    }

    /// Make room for `off`: a narrow list whose base or span cannot hold it is re-based on the
    /// least offset it would hold, or kept whole when even that span passes `u32::MAX`.
    fn fit(&mut self, off: u64) {
        let Offs::Narrow { base, v } = self else {
            return;
        };
        if off >= *base && off - *base <= u64::from(u32::MAX) {
            return;
        }
        let mut lo = off;
        let mut hi = off;
        for &d in v.iter() {
            let o = base.saturating_add(u64::from(d));
            lo = lo.min(o);
            hi = hi.max(o);
        }
        if hi - lo <= u64::from(u32::MAX) {
            let old = *base;
            let mut moved = Vec::with_capacity(v.capacity());
            for &d in v.iter() {
                // In range: `old + d` lies in `lo..=hi`, a span that fits a `u32`.
                let o = old.saturating_add(u64::from(d));
                moved.push(u32::try_from(o - lo).unwrap_or(u32::MAX));
            }
            *self = Offs::Narrow { base: lo, v: moved };
        } else {
            *self = Offs::Wide(self.to_vec());
        }
    }

    /// The narrow distance of `off` from `base`, which `fit` made representable.
    fn narrow(base: u64, off: u64) -> u32 {
        u32::try_from(off.saturating_sub(base)).unwrap_or(u32::MAX)
    }

    fn insert(&mut self, j: usize, off: u64) {
        self.fit(off);
        #[cfg(test)]
        tally::moved(self.len().saturating_sub(j));
        match self {
            Offs::Narrow { base, v } => v.insert(j, Offs::narrow(*base, off)),
            Offs::Wide(v) => v.insert(j, off),
        }
    }

    fn set(&mut self, j: usize, off: u64) {
        self.fit(off);
        match self {
            Offs::Narrow { base, v } => {
                if let Some(slot) = v.get_mut(j) {
                    *slot = Offs::narrow(*base, off);
                }
            }
            Offs::Wide(v) => {
                if let Some(slot) = v.get_mut(j) {
                    *slot = off;
                }
            }
        }
    }

    /// Drop the first `k` offsets.
    #[cfg(any(test, feature = "retention"))]
    fn drain_front(&mut self, k: usize) {
        match self {
            Offs::Narrow { v, .. } => drop(v.drain(..k.min(v.len()))),
            Offs::Wide(v) => drop(v.drain(..k.min(v.len()))),
        }
    }

    fn shrink(&mut self, to: usize) {
        match self {
            Offs::Narrow { v, .. } => v.shrink_to(to),
            Offs::Wide(v) => v.shrink_to(to),
        }
    }

    /// The bytes the offsets' vector holds.
    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        match self {
            Offs::Narrow { v, .. } => v.capacity() * std::mem::size_of::<u32>(),
            Offs::Wide(v) => v.capacity() * std::mem::size_of::<u64>(),
        }
    }

    /// Whether the offsets strictly increase, in seq order.
    fn increasing(&self) -> bool {
        let mut prev: Option<u64> = None;
        for j in 0..self.len() {
            let o = self.get(j);
            if let (Some(p), Some(o)) = (prev, o) {
                if o <= p {
                    return false;
                }
            }
            prev = o;
        }
        true
    }
}

/// Whether `seq` is the seq after `prev`.
fn follows(prev: Option<u64>, seq: u64) -> bool {
    match prev {
        Some(p) => p.checked_add(1) == Some(seq),
        None => false,
    }
}

/// At most `chunk_cap()` of one producer's packed keys: a stretch of its seq order that no
/// other chunk of the producer overlaps.
#[derive(Debug)]
struct Chunk {
    /// Maximal runs of consecutive seqs within the chunk, sorted by `seq0`, disjoint, none
    /// empty (a run that crosses into the next chunk is two runs, one in each).
    runs: Vec<Run>,
    /// One offset per key, in seq order.
    offs: Offs,
    /// SOUND, not complete: when set, `offs` strictly increases in seq order, so a cut drops
    /// a prefix. Kept while every insert lands above every offset held before it in the
    /// chunk's order and below every one after it (a producer publishing increasing seqs on a
    /// log appending at its head).
    ordered: bool,
    /// The least seq held, `runs[0].seq0`, kept beside the header so that a search over a
    /// producer's chunks reads no run vector.
    first: u64,
}

impl Chunk {
    /// The chunk holding `pairs`, `(seq, offset)` sorted by seq and distinct, at capacity.
    fn of(pairs: &[(u64, u64)]) -> Chunk {
        let mut n_runs = 0usize;
        let mut prev: Option<u64> = None;
        for &(seq, _) in pairs {
            if !follows(prev, seq) {
                n_runs = n_runs.saturating_add(1);
            }
            prev = Some(seq);
        }
        let mut runs = Vec::with_capacity(n_runs);
        prev = None;
        for (at, &(seq0, _)) in pairs.iter().enumerate() {
            if !follows(prev, seq0) {
                runs.push(Run { seq0, at });
            }
            prev = Some(seq0);
        }
        let offs = Offs::of(pairs.iter().map(|p| p.1), pairs.len());
        let ordered = offs.increasing();
        let first = match runs.first() {
            Some(r) => r.seq0,
            None => 0,
        };
        Chunk {
            runs,
            offs,
            ordered,
            first,
        }
    }

    fn len(&self) -> usize {
        self.offs.len()
    }

    /// The least seq held (a chunk is never left empty).
    fn first_seq(&self) -> u64 {
        self.first
    }

    /// Re-read `first` after the runs changed at their front.
    fn refirst(&mut self) {
        if let Some(r) = self.runs.first() {
            self.first = r.seq0;
        }
    }

    /// The greatest seq held.
    fn last_seq(&self) -> u64 {
        match self.runs.last() {
            Some(r) => {
                let tail = self.len().saturating_sub(r.at).saturating_sub(1);
                r.seq0.saturating_add(tail as u64)
            }
            None => 0,
        }
    }

    /// Where run `i`'s entries end in `offs`.
    fn run_end(&self, i: usize) -> usize {
        match self.runs.get(i.saturating_add(1)) {
            Some(next) => next.at,
            None => self.offs.len(),
        }
    }

    /// Index into `offs` of `seq`, when held.
    fn slot(&self, seq: u64) -> Option<usize> {
        let i = self
            .runs
            .partition_point(|r| r.seq0 <= seq)
            .checked_sub(1)?;
        let run = self.runs.get(i)?;
        let k = usize::try_from(seq - run.seq0).ok()?;
        let j = run.at.checked_add(k)?;
        if j < self.run_end(i) {
            Some(j)
        } else {
            None
        }
    }

    /// Whether entry `j` is the last of its run.
    #[cfg(test)]
    fn ends_run(&self, j: usize) -> bool {
        match self.runs.partition_point(|r| r.at <= j).checked_sub(1) {
            Some(i) => j.saturating_add(1) == self.run_end(i),
            None => false,
        }
    }

    /// Whether `off` placed at index `j` keeps `offs` increasing, with `next` the index of
    /// the entry that would follow it.
    fn keeps_order(&self, j: usize, next: usize, off: u64) -> bool {
        let below = match j.checked_sub(1) {
            Some(p) => self.offs.get(p).is_some_and(|o| o < off),
            None => true,
        };
        let above = match self.offs.get(next) {
            Some(o) => off < o,
            None => true,
        };
        below && above
    }

    /// Shift every run from `from` on by one entry (a key was inserted before them).
    fn shift_from(&mut self, from: usize) {
        #[cfg(test)]
        tally::moved(self.runs.len().saturating_sub(from));
        for r in self.runs.iter_mut().skip(from) {
            r.at = r.at.saturating_add(1);
        }
    }

    /// Replace entry `j`'s offset.
    fn set(&mut self, j: usize, off: u64) {
        self.ordered = self.ordered && self.keeps_order(j, j.saturating_add(1), off);
        self.offs.set(j, off);
    }

    /// Insert `seq`, which the chunk does not hold, at `off`.
    fn insert(&mut self, seq: u64, off: u64) {
        // `i` runs start at or below `seq`; none holds it, so it extends the run before it,
        // starts the run after it, joins the two, or stands alone.
        let i = self.runs.partition_point(|r| r.seq0 <= seq);
        let extends = match i.checked_sub(1) {
            Some(p) => match self.runs.get(p) {
                Some(run) => {
                    let len = self.run_end(p).saturating_sub(run.at) as u64;
                    run.seq0.checked_add(len) == Some(seq)
                }
                None => false,
            },
            None => false,
        };
        let starts_next = match (self.runs.get(i), seq.checked_add(1)) {
            (Some(next), Some(after)) => next.seq0 == after,
            _ => false,
        };
        let j = match self.runs.get(i) {
            Some(next) => next.at,
            None => self.offs.len(),
        };
        self.ordered = self.ordered && self.keeps_order(j, j, off);
        self.offs.insert(j, off);
        match (extends, starts_next) {
            (true, true) => {
                // The run before now runs into the one after: they are one run.
                self.runs.remove(i);
                self.shift_from(i);
            }
            (true, false) => self.shift_from(i),
            (false, true) => {
                if let Some(next) = self.runs.get_mut(i) {
                    next.seq0 = seq;
                }
                self.shift_from(i.saturating_add(1));
            }
            (false, false) => {
                self.runs.insert(i, Run { seq0: seq, at: j });
                self.shift_from(i.saturating_add(1));
            }
        }
        if i == 0 {
            self.refirst();
        }
    }

    /// Call `f` with every `(seq, offset)` held, in seq order, while it answers true;
    /// whether it always did.
    fn each(&self, mut f: impl FnMut(u64, u64) -> bool) -> bool {
        for (i, run) in self.runs.iter().enumerate() {
            for j in run.at..self.run_end(i) {
                let Some(o) = self.offs.get(j) else {
                    return false;
                };
                if !f(run.seq0.saturating_add((j - run.at) as u64), o) {
                    return false;
                }
            }
        }
        true
    }

    /// Every `(seq, offset)` held, in seq order.
    fn pairs(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::with_capacity(self.len());
        self.each(|seq, o| {
            out.push((seq, o));
            true
        });
        out
    }

    /// Take in `from`'s keys, every one of which lies above this chunk's.
    #[cfg(any(test, feature = "retention"))]
    fn absorb(&mut self, from: &Chunk) {
        let mut pairs = Vec::with_capacity(self.len().saturating_add(from.len()));
        for c in [&*self, from] {
            c.each(|seq, o| {
                pairs.push((seq, o));
                true
            });
        }
        #[cfg(test)]
        tally::scratch(pairs.capacity() * std::mem::size_of::<(u64, u64)>());
        *self = Chunk::of(&pairs);
    }

    /// Drop every key whose offset lies below `min`; how many were dropped.
    #[cfg(any(test, feature = "retention"))]
    fn retain_from(&mut self, min: u64) -> usize {
        let n = self.len();
        let cut = |o: u64| !crate::store::dedup_retains_from_cut(o, min);
        if self.ordered {
            // The offsets rise in seq order, so the keys below the cut are a prefix.
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if self.offs.get(mid).is_some_and(cut) {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            let k = lo;
            if k == 0 {
                return 0;
            }
            // The run holding entry `k` becomes the first; the ones wholly below go.
            let first = self.runs.partition_point(|r| r.at <= k).saturating_sub(1);
            self.runs.drain(..first.min(self.runs.len()));
            if k == n {
                self.runs.clear();
            } else if let Some(run) = self.runs.first_mut() {
                run.seq0 = run.seq0.saturating_add((k - run.at) as u64);
                run.at = k;
            }
            for r in &mut self.runs {
                r.at -= k;
            }
            self.offs.drain_front(k);
            self.refirst();
            return k;
        }
        // Unordered: count first, so a chunk the cut misses copies nothing, then rebuild
        // from the kept keys. A dropped entry inside a run splits it, so the runs stay
        // maximal.
        let mut gone = 0usize;
        for j in 0..n {
            if self.offs.get(j).is_some_and(cut) {
                gone = gone.saturating_add(1);
            }
        }
        if gone == 0 {
            return 0;
        }
        let mut kept = Vec::with_capacity(n - gone);
        self.each(|seq, o| {
            if !cut(o) {
                kept.push((seq, o));
            }
            true
        });
        #[cfg(test)]
        tally::scratch(kept.capacity() * std::mem::size_of::<(u64, u64)>());
        *self = Chunk::of(&kept);
        gone
    }

    fn shrink_to_fit(&mut self) {
        self.runs.shrink_to_fit();
        self.offs.shrink(0);
    }
}

/// One producer's packed keys: chunks sorted by seq, their stretches disjoint, none empty.
#[derive(Debug, Default)]
struct Packed {
    chunks: Vec<Chunk>,
}

impl Packed {
    /// The chunk whose stretch of the order `seq` falls in: the last that starts at or below
    /// it, or the first.
    fn at(&self, seq: u64) -> usize {
        self.chunks
            .partition_point(|c| c.first_seq() <= seq)
            .saturating_sub(1)
    }

    /// The chunk and entry of `seq`, when held.
    fn slot(&self, seq: u64) -> Option<(usize, usize)> {
        let i = self.at(seq);
        let j = self.chunks.get(i)?.slot(seq)?;
        Some((i, j))
    }

    fn get(&self, seq: u64) -> Option<u64> {
        let (i, j) = self.slot(seq)?;
        self.chunks.get(i)?.offs.get(j)
    }

    /// Place `seq` at `off`: replace its offset when held, else insert it; whether it is new.
    /// A new key joins the chunk whose stretch holds its seq. A full chunk passes a key past
    /// its end to the next chunk when that one has room, else to a new chunk, and splits in
    /// halves for a key inside it.
    fn put(&mut self, seq: u64, off: u64) -> bool {
        let cap = chunk_cap();
        let i = self.at(seq);
        let (len, first, last) = match self.chunks.get_mut(i) {
            Some(c) => {
                if let Some(j) = c.slot(seq) {
                    c.set(j, off);
                    return false;
                }
                (c.len(), c.first_seq(), c.last_seq())
            }
            None => {
                self.chunks.push(Chunk::of(&[(seq, off)]));
                return true;
            }
        };
        if len < cap {
            if let Some(c) = self.chunks.get_mut(i) {
                c.insert(seq, off);
            }
            return true;
        }
        if seq > last {
            let next = i.saturating_add(1);
            if let Some(c) = self.chunks.get_mut(next) {
                if c.len() < cap {
                    c.insert(seq, off);
                    return true;
                }
            }
            self.open(next, seq, off);
            return true;
        }
        if seq < first {
            // Below the first chunk.
            self.open(i, seq, off);
            return true;
        }
        // Inside a full chunk: split it in halves and place the key in the half whose
        // stretch holds it.
        let Some(c) = self.chunks.get_mut(i) else {
            return true;
        };
        let pairs = c.pairs();
        let (lo, hi) = pairs.split_at(pairs.len() / 2);
        let upper_first = hi.first().map(|p| p.0);
        *c = Chunk::of(lo);
        #[cfg(test)]
        tally::moved(pairs.len() + self.chunks.len().saturating_sub(i));
        self.chunks.insert(i.saturating_add(1), Chunk::of(hi));
        let into = if upper_first.is_some_and(|f| seq > f) {
            i.saturating_add(1)
        } else {
            i
        };
        if let Some(c) = self.chunks.get_mut(into) {
            c.insert(seq, off);
        }
        true
    }

    /// A new chunk at `i` holding just `seq`.
    fn open(&mut self, i: usize, seq: u64, off: u64) {
        #[cfg(test)]
        tally::moved(self.chunks.len().saturating_sub(i));
        self.chunks.insert(i, Chunk::of(&[(seq, off)]));
    }

    /// Drop every key whose offset lies below `min`; how many were dropped. Emptied chunks
    /// go, and neighbours that together hold at most half a chunk join, so a cut that thins
    /// chunks leaves no chunk header to a key or two.
    #[cfg(any(test, feature = "retention"))]
    fn retain_from(&mut self, min: u64) -> usize {
        let mut dropped = 0usize;
        for c in &mut self.chunks {
            dropped = dropped.saturating_add(c.retain_from(min));
        }
        if dropped == 0 {
            return 0;
        }
        let half = chunk_cap() / 2;
        // In place: `..w` are the chunks kept so far, `w..r` the emptied or absorbed.
        let mut w = 0usize;
        for r in 0..self.chunks.len() {
            let len = match self.chunks.get(r) {
                Some(c) if c.len() > 0 => c.len(),
                _ => continue,
            };
            let joins = match w.checked_sub(1).and_then(|p| self.chunks.get(p)) {
                Some(prev) => prev.len().saturating_add(len) <= half,
                None => false,
            };
            if joins {
                let (kept, rest) = self.chunks.split_at_mut(r);
                if let (Some(into), Some(from)) = (kept.get_mut(w - 1), rest.first()) {
                    into.absorb(from);
                }
            } else {
                self.chunks.swap(w, r);
                w += 1;
            }
        }
        self.chunks.truncate(w);
        if self.chunks.capacity() > 16 && self.chunks.len() <= self.chunks.capacity() / 4 {
            self.chunks
                .shrink_to(self.chunks.len().saturating_mul(2).max(4));
        }
        dropped
    }
}

/// The retained produces' exactly-once keys: `(producer_id, producer_seq) -> Offset`.
#[derive(Debug, Default)]
pub(crate) struct DedupIndex {
    /// The keys held beside a neighbour of their producer, by producer.
    packed: HashMap<u64, Packed>,
    /// The keys with no neighbour held, one entry each, as the map's were.
    loose: HashMap<(u64, u64), u64>,
    len: usize,
    /// The negative control's mutant: a lookup forgets the last key of every packed run.
    #[cfg(test)]
    forget_run_end: bool,
}

impl DedupIndex {
    pub(crate) fn new() -> DedupIndex {
        DedupIndex::default()
    }

    /// The offset `(producer_id, producer_seq)` first landed at, while its record is retained.
    pub(crate) fn get(&self, producer_id: u64, producer_seq: u64) -> Option<Offset> {
        #[cfg(test)]
        if self.forget_run_end {
            let p = self.packed.get(&producer_id);
            if let Some((i, j)) = p.and_then(|p| p.slot(producer_seq)) {
                if p.and_then(|p| p.chunks.get(i))
                    .is_some_and(|c| c.ends_run(j))
                {
                    return None;
                }
            }
        }
        self.lookup(producer_id, producer_seq).map(Offset)
    }

    /// The offset held for a key, in whichever store holds it.
    fn lookup(&self, producer_id: u64, producer_seq: u64) -> Option<u64> {
        if let Some(p) = self.packed.get(&producer_id) {
            if let Some(o) = p.get(producer_seq) {
                return Some(o);
            }
        }
        self.loose.get(&(producer_id, producer_seq)).copied()
    }

    /// Whether `(producer_id, producer_seq)` is held.
    pub(crate) fn contains(&self, producer_id: u64, producer_seq: u64) -> bool {
        self.get(producer_id, producer_seq).is_some()
    }

    /// Record that `(producer_id, producer_seq)` landed at `off`, replacing the offset a key
    /// already held had (as `HashMap::insert` does).
    pub(crate) fn insert(&mut self, producer_id: u64, producer_seq: u64, off: Offset) {
        let (pid, seq, off) = (producer_id, producer_seq, off.0);
        // A key lives in one store: a loose one is replaced where it is. (A log of one
        // consecutive producer holds no loose key, so its inserts hash nothing here.)
        if !self.loose.is_empty() {
            if let Some(o) = self.loose.get_mut(&(pid, seq)) {
                *o = off;
                return;
            }
        }
        // A new key's loose neighbours join it in its producer's packed chunks.
        let near = [seq.checked_sub(1), seq.checked_add(1)];
        if let Some(p) = self.packed.get_mut(&pid) {
            if p.put(seq, off) {
                self.len = self.len.saturating_add(1);
                if !self.loose.is_empty() {
                    for n in near.into_iter().flatten() {
                        if let Some(o) = self.loose.remove(&(pid, n)) {
                            p.put(n, o);
                        }
                    }
                }
            }
            return;
        }
        self.len = self.len.saturating_add(1);
        let mut beside: [Option<(u64, u64)>; 2] = [None, None];
        if !self.loose.is_empty() {
            for (b, n) in beside.iter_mut().zip(near) {
                if let Some(n) = n {
                    *b = self.loose.remove(&(pid, n)).map(|o| (n, o));
                }
            }
        }
        if beside.iter().all(Option::is_none) {
            self.loose.insert((pid, seq), off);
            return;
        }
        let p = self.packed.entry(pid).or_default();
        for (n, o) in beside.into_iter().flatten() {
            p.put(n, o);
        }
        p.put(seq, off);
    }

    /// How many keys are held.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Whether no key is held.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Drop every key whose offset lies below `min` (a retention cut to `min`), and release
    /// the memory a store that shrank no longer uses.
    #[cfg(any(test, feature = "retention"))]
    pub(crate) fn retain_from(&mut self, min: u64) {
        let mut dropped = 0usize;
        for p in self.packed.values_mut() {
            dropped = dropped.saturating_add(p.retain_from(min));
        }
        self.packed.retain(|_, p| !p.chunks.is_empty());
        let before = self.loose.len();
        self.loose
            .retain(|_, o| crate::store::dedup_retains_from_cut(*o, min));
        dropped = dropped.saturating_add(before - self.loose.len());
        self.len = self.len.saturating_sub(dropped);
        if self.packed.capacity() > 64 && self.packed.len() <= self.packed.capacity() / 4 {
            self.packed
                .shrink_to(self.packed.len().saturating_mul(2).max(16));
        }
        if self.loose.capacity() > 64 && self.loose.len() <= self.loose.capacity() / 4 {
            self.loose
                .shrink_to(self.loose.len().saturating_mul(2).max(16));
        }
    }

    /// Give back every vector's spare capacity (after an open built the index whole).
    pub(crate) fn shrink_to_fit(&mut self) {
        for p in self.packed.values_mut() {
            for c in &mut p.chunks {
                c.shrink_to_fit();
            }
            p.chunks.shrink_to_fit();
        }
        self.packed.shrink_to_fit();
        self.loose.shrink_to_fit();
    }

    /// Call `f` with every held `(producer_id, producer_seq, offset)` while it answers
    /// true; whether it always did.
    fn each(&self, mut f: impl FnMut(u64, u64, u64) -> bool) -> bool {
        for (&pid, p) in &self.packed {
            for c in &p.chunks {
                if !c.each(|seq, o| f(pid, seq, o)) {
                    return false;
                }
            }
        }
        for (&(pid, seq), &o) in &self.loose {
            if !f(pid, seq, o) {
                return false;
            }
        }
        true
    }

    /// Every held key and its offset, sorted.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<((u64, u64), u64)> {
        let mut out = Vec::with_capacity(self.len);
        self.each(|pid, seq, o| {
            out.push(((pid, seq), o));
            true
        });
        out.sort_unstable();
        out
    }

    /// The heap the index holds, by its maps' and vectors' capacities.
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        let mut b = tally::map_bytes(
            self.loose.capacity(),
            std::mem::size_of::<((u64, u64), u64)>(),
        );
        b += tally::map_bytes(self.packed.capacity(), std::mem::size_of::<(u64, Packed)>());
        for p in self.packed.values() {
            b += p.chunks.capacity() * std::mem::size_of::<Chunk>();
            for c in &p.chunks {
                b += c.runs.capacity() * std::mem::size_of::<Run>() + c.offs.heap_bytes();
            }
        }
        b
    }

    /// The mutant the negative control runs: every lookup forgets the last key of its run.
    #[cfg(test)]
    pub(crate) fn mutant_forgetting_run_ends() -> DedupIndex {
        DedupIndex {
            forget_run_end: true,
            ..DedupIndex::default()
        }
    }
}

/// Two indexes are equal when they hold the same keys at the same offsets, however each one
/// happens to store them: which keys are packed and where its chunks split depend on the
/// order the keys came in and the cuts since.
impl PartialEq for DedupIndex {
    fn eq(&self, other: &DedupIndex) -> bool {
        // Keys are distinct in each, so equal counts and every key of one held in the other
        // at its offset make the sets equal.
        self.len == other.len && self.each(|pid, seq, o| other.lookup(pid, seq) == Some(o))
    }
}

/// What the tests count of the index's work, per thread: the entries an insert moved within
/// its vectors, and the most scratch one retention cut built. A test may also lower the
/// chunk size on its own thread.
#[cfg(test)]
mod tally {
    use std::cell::Cell;

    thread_local! {
        static MOVED: Cell<u64> = const { Cell::new(0) };
        static SCRATCH: Cell<usize> = const { Cell::new(0) };
        static CHUNK_CAP: Cell<usize> = const { Cell::new(super::CHUNK) };
    }

    pub(super) fn moved(n: usize) {
        MOVED.with(|c| c.set(c.get().saturating_add(n as u64)));
    }

    pub(super) fn scratch(bytes: usize) {
        SCRATCH.with(|c| c.set(c.get().max(bytes)));
    }

    /// The entries moved and the most scratch since the last take, and start again.
    pub(super) fn take() -> (u64, usize) {
        (MOVED.with(|c| c.replace(0)), SCRATCH.with(|c| c.replace(0)))
    }

    pub(super) fn chunk_cap() -> usize {
        CHUNK_CAP.with(Cell::get)
    }

    /// Lower (or restore) this thread's chunk size.
    pub(super) fn set_chunk_cap(cap: usize) {
        CHUNK_CAP.with(|c| c.set(cap.max(2)));
    }

    /// A `HashMap` of `cap` usable entries of `entry` bytes: its buckets (a power of two at
    /// most 7/8 full) and a control byte each.
    pub(super) fn map_bytes(cap: usize, entry: usize) -> usize {
        if cap == 0 {
            return 0;
        }
        let buckets = (cap.saturating_mul(8) / 7).next_power_of_two();
        buckets * (entry + 1)
    }
}

#[cfg(test)]
mod tests {
    //! The index against the map it replaced, over random publish streams.

    use super::*;
    use crate::store::ACK_SEQ_BASE;

    /// xorshift64*: a tiny deterministic PRNG (std-only, reproducible per seed).
    struct Rng(u64);
    impl Rng {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n.max(1)
        }
    }

    /// One producer's habit: how it picks its next seq.
    #[derive(Clone, Copy)]
    enum Habit {
        /// `1, 2, 3, …`
        Consecutive,
        /// Increasing, with gaps.
        Gapped,
        /// Anywhere in a small range: re-sends, backfills and repeats.
        Scattered,
        /// Near `u64::MAX`, where `seq + 1` overflows.
        Top,
        /// Alternately its own consecutive seqs and acks (`ACK_SEQ_BASE | input`) of
        /// inputs that sometimes run.
        Acking,
    }

    /// The broker's two ways of meeting a key: a publish answers a held key from the index
    /// and inserts only a new one; a replica's record and an open's rebuild insert every
    /// record, replacing a held key's offset.
    fn publish(
        idx: &mut DedupIndex,
        map: &mut HashMap<(u64, u64), Offset>,
        key: (u64, u64),
        head: &mut u64,
        replace: bool,
    ) -> Result<(), String> {
        let got = idx.get(key.0, key.1);
        let want = map.get(&key).copied();
        if got != want {
            return Err(format!("get{key:?}: index {got:?}, map {want:?}"));
        }
        if want.is_none() || replace {
            idx.insert(key.0, key.1, Offset(*head));
            map.insert(key, Offset(*head));
        }
        *head += 1;
        Ok(())
    }

    fn compare(idx: &DedupIndex, map: &HashMap<(u64, u64), Offset>) -> Result<(), String> {
        if idx.len() != map.len() {
            return Err(format!("len: index {}, map {}", idx.len(), map.len()));
        }
        for (&(pid, seq), &off) in map {
            let got = idx.get(pid, seq);
            if got != Some(off) {
                return Err(format!("get({pid}, {seq}): index {got:?}, map {off:?}"));
            }
            for probe in [seq.wrapping_add(1), seq.wrapping_sub(1)] {
                let (g, w) = (idx.get(pid, probe), map.get(&(pid, probe)).copied());
                if g != w {
                    return Err(format!("get({pid}, {probe}): index {g:?}, map {w:?}"));
                }
            }
        }
        let mut want: Vec<((u64, u64), u64)> = map.iter().map(|(k, o)| (*k, o.0)).collect();
        want.sort_unstable();
        if idx.entries() != want {
            return Err("the entries differ".to_string());
        }
        Ok(())
    }

    /// One seeded stream: producers with mixed habits publish, replicas replace, retention
    /// cuts the oldest offsets, and an open rebuilds the index from the retained records in
    /// offset order; after every step the index answers every question the map answers.
    fn stream(seed: u64, make: fn() -> DedupIndex) -> Result<(), String> {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut idx = make();
        let mut map: HashMap<(u64, u64), Offset> = HashMap::new();
        let habits = [
            Habit::Consecutive,
            Habit::Gapped,
            Habit::Scattered,
            Habit::Top,
            Habit::Acking,
        ];
        let producers = 1 + rng.below(6);
        let mut next_seq: Vec<u64> = (0..producers).map(|_| 1 + rng.below(4)).collect();
        let habit: Vec<Habit> = (0..producers)
            .map(|_| habits[rng.below(habits.len() as u64) as usize])
            .collect();
        let mut input: Vec<u64> = (0..producers).map(|_| rng.below(1 << 20)).collect();
        let mut head = rng.below(1 << 40);
        let mut base = head;
        for step in 0..600 {
            let roll = rng.below(100);
            if roll < 80 {
                let p = rng.below(producers) as usize;
                let seq = match habit[p] {
                    Habit::Consecutive => next_seq[p],
                    Habit::Gapped => next_seq[p] + rng.below(3),
                    Habit::Scattered => rng.below(64),
                    Habit::Top => u64::MAX - rng.below(8),
                    Habit::Acking => {
                        if rng.below(2) == 0 {
                            next_seq[p]
                        } else {
                            input[p] += 1 + rng.below(2);
                            ACK_SEQ_BASE | input[p]
                        }
                    }
                };
                if seq < ACK_SEQ_BASE || !matches!(habit[p], Habit::Acking) {
                    next_seq[p] = seq.saturating_add(1);
                }
                // A duplicate of a held key, now and then, from any producer.
                let key = if rng.below(10) == 0 && !map.is_empty() {
                    let mut all: Vec<(u64, u64)> = map.keys().copied().collect();
                    all.sort_unstable();
                    all[rng.below(all.len() as u64) as usize]
                } else {
                    (p as u64 * 1_000_003, seq)
                };
                let replace = rng.below(8) == 0;
                publish(&mut idx, &mut map, key, &mut head, replace)
                    .map_err(|e| format!("seed {seed} step {step}: {e}"))?;
            } else if roll < 88 {
                // A jump in the head: other records (wills, commits) took offsets.
                head += rng.below(1 << 33);
            } else if roll < 96 {
                // A retention cut to somewhere between the base and the head.
                base += rng.below(head.saturating_sub(base) + 1);
                idx.retain_from(base);
                map.retain(|_, off| off.0 >= base);
            } else {
                // An open: rebuilt from the retained keys in offset order.
                let mut by_off: Vec<(Offset, (u64, u64))> =
                    map.iter().map(|(k, o)| (*o, *k)).collect();
                by_off.sort_unstable();
                let mut again = make();
                for (off, (pid, seq)) in by_off {
                    again.insert(pid, seq, off);
                }
                again.shrink_to_fit();
                if again != idx {
                    return Err(format!(
                        "seed {seed} step {step}: the rebuilt index differs"
                    ));
                }
                idx = again;
            }
            compare(&idx, &map).map_err(|e| format!("seed {seed} step {step}: {e}"))?;
        }
        Ok(())
    }

    /// Every seed, at the shipped chunk size and at sizes small enough that a stream of a
    /// few hundred keys fills, splits, opens and joins chunks.
    #[test]
    fn the_index_answers_every_question_the_map_answers() {
        for cap in [CHUNK, 2, 3, 8] {
            tally::set_chunk_cap(cap);
            for seed in 0..400 {
                if let Err(e) = stream(seed, DedupIndex::new) {
                    panic!("chunk {cap}: {e}");
                }
            }
        }
    }

    /// The negative control: an index that forgets the last key of each run is caught.
    #[test]
    fn an_index_that_forgets_a_runs_last_key_is_caught() {
        for cap in [CHUNK, 3] {
            tally::set_chunk_cap(cap);
            let mut caught = 0;
            for seed in 0..400 {
                if stream(seed, DedupIndex::mutant_forgetting_run_ends).is_err() {
                    caught += 1;
                }
            }
            assert_eq!(caught, 400, "chunk {cap}: every seed catches the mutant");
        }
    }

    #[test]
    fn a_consecutive_producer_costs_four_bytes_a_key_and_its_chunks_one_run_each() {
        let mut idx = DedupIndex::new();
        let mut off = 0u64;
        for seq in 1..=10_000u64 {
            for pid in 0..4u64 {
                idx.insert(pid, seq, Offset(off));
                off += 1;
            }
        }
        idx.shrink_to_fit();
        assert!(idx.loose.is_empty(), "every key runs beside another");
        for p in idx.packed.values() {
            assert_eq!(p.chunks.len(), 10_000usize.div_ceil(CHUNK));
            let mut keys = 0;
            for c in &p.chunks {
                assert_eq!(c.runs.len(), 1);
                assert!(c.ordered);
                assert!(matches!(&c.offs, Offs::Narrow { v, .. } if v.capacity() == v.len()));
                keys += c.len();
            }
            assert_eq!(keys, 10_000);
        }
        let per_key = idx.heap_bytes() as f64 / 40_000.0;
        assert!(per_key < 4.25, "{per_key} B a key");
        assert_eq!(idx.get(3, 10_000), Some(Offset(off - 1)));
        assert_eq!(idx.get(0, 10_001), None);
    }

    /// A producer that publishes and acks under one id (an ack's seq is
    /// `ACK_SEQ_BASE | input`, above every ordinary seq) inserts its ordinary keys below
    /// every ack key it holds. Each insert moves a bounded number of entries, whether its
    /// acks are sparse (runs of one) or run, live or rebuilt by an open in offset order.
    #[test]
    fn a_producer_that_publishes_and_acks_moves_a_bounded_number_of_entries_a_key() {
        const N: u64 = 20_000;
        for stride in [1u64, 3] {
            let mut idx = DedupIndex::new();
            let mut map: HashMap<(u64, u64), Offset> = HashMap::new();
            let _ = tally::take();
            let mut off = 0u64;
            for k in 1..=N {
                for seq in [k, ACK_SEQ_BASE | (k * stride)] {
                    idx.insert(7, seq, Offset(off));
                    map.insert((7, seq), Offset(off));
                    off += 1;
                }
            }
            let (moved, _) = tally::take();
            let bound = 2 * N * 2 * CHUNK as u64;
            assert!(
                moved <= bound,
                "stride {stride}: {moved} entries moved for {} keys, over {bound}",
                2 * N
            );
            if let Err(e) = compare(&idx, &map) {
                panic!("stride {stride}: {e}");
            }
            let mut by_off: Vec<(Offset, (u64, u64))> = map.iter().map(|(k, o)| (*o, *k)).collect();
            by_off.sort_unstable();
            let mut again = DedupIndex::new();
            for (o, (pid, seq)) in by_off {
                again.insert(pid, seq, o);
            }
            let (moved, _) = tally::take();
            assert!(
                moved <= bound,
                "stride {stride}: the rebuild moved {moved} entries, over {bound}"
            );
            assert!(again == idx, "stride {stride}: the rebuild differs");
        }
    }

    /// Producers that publish one key each (`asb pub` with no `--id`: its process id and a
    /// nanosecond seq) cost no more than the map's one entry a key.
    #[test]
    fn one_key_producers_hold_no_more_than_the_map() {
        let mut idx = DedupIndex::new();
        let mut map: HashMap<(u64, u64), Offset> = HashMap::new();
        for pid in 0..50_000u64 {
            let seq = 1_793_000_000_000_000_000 + pid * 7_919;
            idx.insert(pid, seq, Offset(pid));
            map.insert((pid, seq), Offset(pid));
        }
        idx.shrink_to_fit();
        map.shrink_to_fit();
        let want = tally::map_bytes(map.capacity(), std::mem::size_of::<((u64, u64), Offset)>());
        assert!(
            idx.heap_bytes() <= want,
            "the index holds {} B for 50,000 one-key producers, the map {want}",
            idx.heap_bytes()
        );
        if let Err(e) = compare(&idx, &map) {
            panic!("{e}");
        }
    }

    /// A retention cut builds no scratch beyond one chunk's keys: not when one stray key
    /// (an ack's) sits mid-log, nor when a re-sent old seq lands inside the order with an
    /// offset above its neighbours'.
    #[test]
    fn a_cut_builds_no_scratch_beyond_a_chunk() {
        const N: u64 = 200_000;
        let mut idx = DedupIndex::new();
        let mut map: HashMap<(u64, u64), Offset> = HashMap::new();
        let mut off = 0u64;
        for seq in 1..=N {
            idx.insert(1, seq, Offset(off));
            map.insert((1, seq), Offset(off));
            off += 1;
            if seq == N / 2 {
                idx.insert(1, ACK_SEQ_BASE | 5, Offset(off));
                map.insert((1, ACK_SEQ_BASE | 5), Offset(off));
                off += 1;
            }
        }
        let bound = 16 * CHUNK;
        for (cut, resend) in [(1_000u64, None), (2_000, Some(1_000u64)), (3_000, None)] {
            if let Some(seq) = resend {
                idx.insert(1, seq, Offset(off));
                map.insert((1, seq), Offset(off));
                off += 1;
            }
            let _ = tally::take();
            idx.retain_from(cut);
            map.retain(|_, o| o.0 >= cut);
            let (_, scratch) = tally::take();
            assert!(
                scratch <= bound,
                "the cut to {cut} built {scratch} B of scratch, over {bound}"
            );
            if let Err(e) = compare(&idx, &map) {
                panic!("cut {cut}: {e}");
            }
        }
    }

    #[test]
    fn offsets_spanning_more_than_a_u32_are_kept_whole_and_narrowed_after_a_cut() {
        let mut idx = DedupIndex::new();
        idx.insert(1, 1, Offset(5));
        idx.insert(1, 2, Offset(5 + (1 << 33)));
        let chunk = |idx: &DedupIndex| -> (bool, bool) {
            let c = &idx.packed[&1].chunks[0];
            (matches!(c.offs, Offs::Wide(_)), c.ordered)
        };
        assert_eq!(chunk(&idx), (true, true));
        assert_eq!(idx.get(1, 1), Some(Offset(5)));
        assert_eq!(idx.get(1, 2), Some(Offset(5 + (1 << 33))));
        idx.retain_from(6);
        assert_eq!(idx.get(1, 1), None);
        assert_eq!(idx.get(1, 2), Some(Offset(5 + (1 << 33))));
        assert_eq!(idx.len(), 1);
        // A re-sent 1 lands before 2 at a later offset: the chunk is unordered, and its
        // filter narrows what fits again.
        idx.insert(1, 1, Offset(6 + (1 << 33)));
        assert_eq!(chunk(&idx), (true, false));
        idx.retain_from(6 + (1 << 33));
        assert_eq!(chunk(&idx), (false, true));
        assert_eq!(idx.entries(), vec![((1, 1), 6 + (1 << 33))]);
    }

    #[test]
    fn a_resent_old_seq_lands_inside_the_order_and_joins_its_neighbours() {
        let mut idx = DedupIndex::new();
        for (seq, off) in [(1, 0), (2, 1), (4, 2), (5, 3)] {
            idx.insert(9, seq, Offset(off));
        }
        let runs = |idx: &DedupIndex| idx.packed[&9].chunks[0].runs.clone();
        assert_eq!(runs(&idx).len(), 2);
        assert!(idx.loose.is_empty());
        idx.insert(9, 3, Offset(4));
        assert_eq!(runs(&idx).len(), 1, "3 joins 1..=2 and 4..=5");
        assert!(!idx.packed[&9].chunks[0].ordered);
        assert_eq!(idx.get(9, 3), Some(Offset(4)));
        idx.retain_from(2);
        assert_eq!(
            idx.entries(),
            vec![((9, 3), 4), ((9, 4), 2), ((9, 5), 3)],
            "the cut drops 1 and 2 and keeps the back-filled 3"
        );
        assert_eq!(runs(&idx), vec![Run { seq0: 3, at: 0 }]);
    }

    /// A producer none of whose keys neighbour keeps them loose; a key beside one packs both,
    /// a key between two loose ones packs all three as one run, and from then on the
    /// producer's sparse keys are packed too, while another producer's stay loose.
    #[test]
    fn a_lone_key_stays_loose_until_a_neighbour_packs_it() {
        let mut idx = DedupIndex::new();
        idx.insert(4, 10, Offset(0));
        idx.insert(4, 12, Offset(1));
        assert_eq!(idx.loose.len(), 2);
        assert!(idx.packed.is_empty());
        idx.insert(4, 11, Offset(2));
        assert!(idx.loose.is_empty());
        assert_eq!(idx.packed[&4].chunks[0].runs, vec![Run { seq0: 10, at: 0 }]);
        assert_eq!(
            idx.entries(),
            vec![((4, 10), 0), ((4, 11), 2), ((4, 12), 1)]
        );
        // A sparse key of the packed producer is a run of one; another producer's lone key
        // stays loose; a replace stays where the key is.
        idx.insert(4, 20, Offset(3));
        idx.insert(5, 20, Offset(4));
        idx.insert(4, 20, Offset(5));
        idx.insert(5, 20, Offset(6));
        idx.insert(4, 11, Offset(7));
        assert_eq!(
            idx.packed[&4].chunks[0].runs,
            vec![Run { seq0: 10, at: 0 }, Run { seq0: 20, at: 3 }]
        );
        assert_eq!(idx.loose.get(&(5, 20)), Some(&6));
        assert_eq!(idx.loose.len(), 1);
        assert_eq!(idx.get(4, 20), Some(Offset(5)));
        assert_eq!(idx.get(4, 11), Some(Offset(7)));
        assert_eq!(idx.len(), 5);
    }
}
