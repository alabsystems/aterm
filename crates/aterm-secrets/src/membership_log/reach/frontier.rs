// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Frontiers: for indexed records sorted into disjoint groups (a register's ops, an audience's
//! epochs), each grouped record's frontier, the records of its own group newest in its strict
//! past, from one pass over the index.
//!
//! The pass takes the records in order and gives each one it will read again its dominated set:
//! the grouped records its causal past holds below another record of their own group, there. A
//! record's dominated set is its parents' joined, and a grouped record's adds every record of its
//! own group in its past, since it follows them all. A grouped record's frontier is the records
//! of its group in its past less what its parents dominate: one a parent dominates lies below a
//! record of its group in this record's past, and one below such a record lies in the past of
//! the parent that holds it.
//!
//! - Work: a record's dominated set ORs its parents' sets (at most `ceil(j / 64)` words for a
//!   parent at `j`, the cost the index paid to hold the record), and a grouped record reads its
//!   own row against its group (a mask, or a bit test for each earlier record of a group of
//!   fewer than [`DENSE`]) and then its frontier, each at most its row's words. So a pass costs
//!   about the index's own build over the records it reads, plus three rows per grouped record:
//!   a register of `k` ops in a log of about `k` records costs `O(k^2 / 64)` words, where testing
//!   each op's past pair by pair cost `O(k^3)`.
//! - Memory: only the grouped records and the records some grouped record follows are read, each
//!   record's set is let go once its last child in the pass has read it, a record adding nothing
//!   to its one parent's set shares that set, and an empty set takes nothing.

#[cfg(test)]
use super::{MUTANT, Mutant};
use super::{Reach, tally};
use std::rc::Rc;

/// Groups of this many records or more read their members from a mask of the group; smaller
/// ones test each earlier member's bit, which costs less than a mask's words.
const DENSE: usize = 64;

/// No group, or no later reader.
const NONE: u32 = u32::MAX;

/// Where a grouped record sits: its group and its position within the group.
#[derive(Clone, Copy)]
struct Slot {
    group: u32,
    position: u32,
}

/// A grouped record's frontier, as [`Reach::frontiers`] hands it out: a set over the index whose
/// word 0 is the index's word `start`, with no empty word first or last.
pub(in crate::membership_log) struct Frontier<'p> {
    words: &'p [u64],
    start: usize,
    slots: &'p [Slot],
}

impl Frontier<'_> {
    /// Its records' positions within their group, ascending.
    pub(in crate::membership_log) fn positions(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for (w, &word) in self.words.iter().enumerate() {
            let mut rest = word;
            while rest != 0 {
                let at = (self.start + w) * 64 + rest.trailing_zeros() as usize;
                rest &= rest - 1;
                out.push(self.slots[at].position as usize);
            }
        }
        tally(self.words.len(), 0, out.len());
        out
    }

    /// Its first word's place in the index and its words, for telling one frontier from another
    /// of the same pass: two are the same set exactly when both are equal.
    pub(in crate::membership_log) const fn words(&self) -> (usize, &[u64]) {
        (self.start, self.words)
    }
}

/// `set` less its trailing empty words.
fn trimmed(mut set: Vec<u64>) -> Vec<u64> {
    while set.last() == Some(&0) {
        set.pop();
    }
    set
}

impl Reach {
    /// Walk the records of `groups` (each group's indexed records, ascending; no record in two
    /// groups) in record order, and hand `visit` each one's group, its position there and its
    /// frontier: the records of its own group in its strict past that lie in no other such
    /// record's past. `parents` gives each indexed record's parents, as the index held them.
    ///
    /// # Panics
    /// A grouped record is not indexed or sits in two groups; there are more groups, or a group
    /// holds more records, than a `u32` counts, or a record the pass reads has an index a `u32`
    /// does not hold; or (in a debug build) a group is not ascending.
    pub(in crate::membership_log) fn frontiers<P, I>(
        &self,
        groups: &[Vec<usize>],
        parents: P,
        mut visit: impl FnMut(usize, usize, &Frontier<'_>),
    ) where
        P: Fn(usize) -> I,
        I: IntoIterator<Item = usize>,
    {
        let mut slots = vec![
            Slot {
                group: NONE,
                position: NONE,
            };
            self.len
        ];
        let mut members = Vec::new();
        for (g, group) in groups.iter().enumerate() {
            debug_assert!(group.windows(2).all(|pair| pair[0] < pair[1]));
            for (position, &x) in group.iter().enumerate() {
                assert!(
                    x < self.len && slots[x].group == NONE,
                    "groups are disjoint sets of indexed records"
                );
                slots[x] = Slot {
                    group: u32::try_from(g).expect("groups fit a u32"),
                    position: u32::try_from(position).expect("a group fits a u32"),
                };
                members.push(x);
            }
        }
        let (Some(&lo), Some(&hi)) = (members.iter().min(), members.iter().max()) else {
            return;
        };
        let base = lo / 64;
        let masks: Vec<Option<Vec<u64>>> = groups
            .iter()
            .map(|group| {
                let last = *group.last()?;
                (group.len() >= DENSE).then(|| {
                    let mut mask = vec![0; last / 64 + 1 - base];
                    for &x in group {
                        mask[x / 64 - base] |= 1 << (x % 64);
                    }
                    mask
                })
            })
            .collect();
        // Only a grouped record and a record some grouped record follows are read, each until
        // its last child among them: one pass back over the parents finds both.
        let mut read = vec![false; hi + 1 - lo];
        let mut last = vec![NONE; hi + 1 - lo];
        for x in (lo..=hi).rev() {
            if slots[x].group == NONE && !read[x - lo] {
                continue;
            }
            read[x - lo] = true;
            for p in parents(x) {
                tally(0, 0, 1);
                if p >= lo && !read[p - lo] {
                    read[p - lo] = true;
                    last[p - lo] = u32::try_from(x).expect("an index fits a u32");
                }
            }
        }
        let mut sets: Vec<Option<Rc<[u64]>>> = vec![None; hi + 1 - lo];
        let mut held: Vec<usize> = Vec::new();
        for x in (lo..=hi).filter(|&x| read[x - lo]) {
            held.clear();
            held.extend(parents(x).into_iter().filter(|&p| p >= lo));
            // The parents' dominated sets, each once.
            let mut from: Vec<Rc<[u64]>> = Vec::new();
            for &p in &held {
                if let Some(set) = &sets[p - lo]
                    && !from.iter().any(|other| Rc::ptr_eq(other, set))
                {
                    from.push(Rc::clone(set));
                }
            }
            let joined = |into: &mut Vec<u64>| {
                for set in &from {
                    if into.len() < set.len() {
                        into.resize(set.len(), 0);
                    }
                    tally(set.len(), 0, 0);
                    for (word, more) in into.iter_mut().zip(set.iter()) {
                        *word |= more;
                    }
                }
            };
            let kept = last[x - lo] != NONE;
            let slot = slots[x];
            if slot.group == NONE {
                if kept {
                    sets[x - lo] = match from.as_slice() {
                        [] => None,
                        [one] => Some(Rc::clone(one)),
                        _ => {
                            let mut set = Vec::new();
                            joined(&mut set);
                            Some(trimmed(set).into())
                        }
                    };
                }
            } else {
                let g = slot.group as usize;
                let position = slot.position as usize;
                let width = x.div_ceil(64).saturating_sub(base);
                let mut dom = Vec::new();
                #[cfg(test)]
                let ignore = MUTANT.get() == Some(Mutant::IgnoreParents);
                #[cfg(not(test))]
                let ignore = false;
                if !ignore {
                    joined(&mut dom);
                }
                dom.resize(width.max(dom.len()), 0);
                let mut own = vec![0; width];
                if let Some(mask) = &masks[g] {
                    let past = self.row(x).get(base..).unwrap_or_default();
                    tally(past.len(), 0, 0);
                    for ((word, more), group) in own.iter_mut().zip(past).zip(mask) {
                        *word = more & group;
                    }
                } else {
                    tally(0, position, 0);
                    for &b in &groups[g][..position] {
                        if self.before(b, x) {
                            own[b / 64 - base] |= 1 << (b % 64);
                        }
                    }
                }
                tally(width, 0, 0);
                let front = trimmed(own.iter().zip(&dom).map(|(o, d)| o & !d).collect());
                let lead = front.iter().take_while(|&&word| word == 0).count();
                visit(
                    g,
                    position,
                    &Frontier {
                        words: &front[lead..],
                        start: base + lead,
                        slots: &slots,
                    },
                );
                if kept {
                    #[cfg(test)]
                    let omit = MUTANT.get() == Some(Mutant::OmitOwnGroup);
                    #[cfg(not(test))]
                    let omit = false;
                    if !omit {
                        tally(width, 0, 0);
                        for (word, more) in dom.iter_mut().zip(&own) {
                            *word |= more;
                        }
                    }
                    let dom = trimmed(dom);
                    sets[x - lo] = (!dom.is_empty()).then(|| dom.into());
                }
            }
            for &p in &held {
                if last[p - lo] as usize == x {
                    sets[p - lo] = None;
                }
            }
        }
    }
}
