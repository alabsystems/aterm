// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The register resolver [`super::Register`] replaced, kept only as the tests' oracle: for every
//! op, a scan of every earlier op with a test against each newest one found so far, and a test
//! of each pair among the newest. It counts its causal tests, and gives up once they pass a
//! budget, so a test can show it breaks a bound without running out the cubic count.
//!
//! Every resolution the tests run checks itself against it ([`check`]), and, up to
//! [`ISLAND_OPS`] ops, against the island's fold too ([`island`], `aterm_mesh_core::convergence`'s
//! `Register`, which law 5's proof reads step for step).

use super::super::{CandidateOp, DecodedOp};
use super::Register;
use crate::membership_log::eval;
use aterm_mesh_core::convergence::{Op, Register as Fold};
use std::cell::Cell;

/// Whether two register ops write the same thing: two `Delete`s, or two `Put`s of one value with
/// the same flags.
fn same_op(a: &DecodedOp, b: &DecodedOp) -> bool {
    match (a, b) {
        (DecodedOp::Delete, DecodedOp::Delete) => true,
        (DecodedOp::Put(a), DecodedOp::Put(b)) => {
            a.if_absent == b.if_absent && a.anchor == b.anchor && a.value_class == b.value_class
        }
        _ => false,
    }
}

/// The budget ran out.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::membership_log) struct Exhausted;

/// What the replaced resolver read off a register: each op's carry and rep, the siblings, their
/// winner, and whether the register is a conflict.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::membership_log) struct Resolved {
    pub(in crate::membership_log) carries: Vec<bool>,
    pub(in crate::membership_log) reps: Vec<usize>,
    pub(in crate::membership_log) siblings: Vec<usize>,
    pub(in crate::membership_log) winner: Option<usize>,
    pub(in crate::membership_log) conflict: bool,
}

struct Old<'a> {
    cands: &'a [CandidateOp],
    carries: Vec<bool>,
    reps: Vec<usize>,
    before: &'a dyn Fn(usize, usize) -> bool,
    tests: Cell<usize>,
    budget: usize,
}

impl Old<'_> {
    fn before(&self, a: usize, b: usize) -> bool {
        self.tests.set(self.tests.get() + 1);
        (self.before)(a, b)
    }

    fn spent(&self) -> Result<(), Exhausted> {
        if self.tests.get() > self.budget {
            Err(Exhausted)
        } else {
            Ok(())
        }
    }

    fn classify(&mut self) -> Result<(), Exhausted> {
        let cands = self.cands;
        for k in 0..cands.len() {
            let mut maximal: Vec<usize> = Vec::new();
            for b in (0..k).rev() {
                if self.before(cands[b].index, cands[k].index)
                    && !maximal
                        .iter()
                        .any(|&m| self.before(cands[b].index, cands[m].index))
                {
                    maximal.push(b);
                }
                self.spent()?;
            }
            let held = self.live(maximal)?;
            let carried = match cands[k].op {
                DecodedOp::Put(_) => self
                    .winner(&held)
                    .filter(|&w| same_op(&cands[w].op, &cands[k].op)),
                DecodedOp::Delete => self.winner(&held).filter(|_| {
                    held.iter()
                        .all(|&h| matches!(cands[h].op, DecodedOp::Delete))
                }),
            };
            if let Some(w) = carried {
                self.carries[k] = true;
                self.reps[k] = self.reps[w];
            }
        }
        Ok(())
    }

    fn live(&self, maximal: Vec<usize>) -> Result<Vec<usize>, Exhausted> {
        let cands = self.cands;
        let mut kept = Vec::new();
        for &c in &maximal {
            if !self.carries[c]
                || !maximal.iter().any(|&s| {
                    s != c
                        && !same_op(&cands[s].op, &cands[c].op)
                        && self.before(cands[self.reps[c]].index, cands[s].index)
                })
            {
                kept.push(c);
            }
            self.spent()?;
        }
        Ok(kept)
    }

    fn winner(&self, siblings: &[usize]) -> Option<usize> {
        siblings
            .iter()
            .copied()
            .min_by_key(|&i| (self.cands[self.reps[i]].record_id, self.cands[i].record_id))
    }

    fn siblings(&self) -> Result<Vec<usize>, Exhausted> {
        let cands = self.cands;
        let mut maximal = Vec::new();
        for a in 0..cands.len() {
            let replaced = (0..cands.len()).any(|b| {
                a != b
                    && self.before(cands[a].index, cands[b].index)
                    && (!self.carries[b] || same_op(&cands[a].op, &cands[b].op))
            });
            if !replaced {
                maximal.push(a);
            }
            self.spent()?;
        }
        self.live(maximal)
    }
}

/// Resolve `cands` (one register's ops, in record order) as the replaced resolver did, with
/// `before` for causal precedence between record indexes, giving up once more than `budget`
/// tests have run; with the tests it ran.
pub(in crate::membership_log) fn resolve(
    cands: &[CandidateOp],
    before: &dyn Fn(usize, usize) -> bool,
    budget: usize,
) -> (Result<Resolved, Exhausted>, usize) {
    let mut old = Old {
        cands,
        carries: vec![false; cands.len()],
        reps: (0..cands.len()).collect(),
        before,
        tests: Cell::new(0),
        budget,
    };
    let resolved = old.classify().and_then(|()| {
        let siblings = old.siblings()?;
        let winner = old.winner(&siblings);
        let conflict = winner.is_some_and(|w| {
            siblings
                .iter()
                .any(|&i| !same_op(&cands[i].op, &cands[w].op))
        });
        Ok(Resolved {
            siblings,
            winner,
            conflict,
            carries: old.carries.clone(),
            reps: old.reps.clone(),
        })
    });
    (resolved, old.tests.get())
}

/// What `register` and its `siblings` resolved, as [`resolve`] gives it.
pub(in crate::membership_log) fn resolved(register: &Register<'_>, siblings: &[usize]) -> Resolved {
    let winner = register.winner(siblings);
    Resolved {
        carries: register.carries.clone(),
        reps: register.reps.clone(),
        siblings: siblings.to_vec(),
        winner,
        conflict: winner.is_some_and(|w| register.conflict(siblings, w)),
    }
}

/// The most ops a register the island's fold resolves holds: the fold asks each pair of ops
/// whether a third lies between them, `O(k^3)` causal tests, so larger registers are held only
/// to [`resolve`].
pub(in crate::membership_log) const ISLAND_OPS: usize = 128;

/// What the island's fold reads off `cands` (one register's ops, in record order), with `before`
/// for causal precedence between record indexes, as [`resolve`] gives it; `None` past
/// [`ISLAND_OPS`] ops. Each op's record id is its rank by `RecordId` (the winner order compares
/// ids) and its class the first op that writes the same thing.
pub(in crate::membership_log) fn island(
    cands: &[CandidateOp],
    before: &dyn Fn(usize, usize) -> bool,
) -> Option<Resolved> {
    let n = cands.len();
    if n > ISLAND_OPS {
        return None;
    }
    let mut by_id: Vec<usize> = (0..n).collect();
    by_id.sort_by_key(|&i| cands[i].record_id);
    let mut rid = vec![0; n];
    for (rank, &i) in by_id.iter().enumerate() {
        rid[i] = rank;
    }
    let ops: Vec<Op> = (0..n)
        .map(|i| Op {
            rid: rid[i],
            class: (0..n)
                .find(|&j| same_op(&cands[j].op, &cands[i].op))
                .unwrap_or(i),
            delete: matches!(cands[i].op, DecodedOp::Delete),
        })
        .collect();
    let fold = Fold::new(&ops, |a, b| {
        before(cands[by_id[a]].index, cands[by_id[b]].index)
    });
    Some(Resolved {
        carries: (0..n).map(|i| fold.carries(i)).collect(),
        reps: (0..n).map(|i| by_id[fold.rep(i)]).collect(),
        siblings: (0..n).filter(|&i| fold.is_sibling(i)).collect(),
        winner: fold.winner(),
        conflict: fold.conflict(),
    })
}

thread_local! {
    /// Registers checked against the oracle on this thread, and how many disagreed.
    pub(in crate::membership_log) static CHECKED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// How many of [`CHECKED`]'s registers the island's fold resolved too.
    pub(in crate::membership_log) static ISLAND_CHECKED: Cell<usize> = const { Cell::new(0) };
    /// Count a disagreement rather than fail on it: a negative control's mutant disagrees.
    pub(in crate::membership_log) static COUNT_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Check what `register` and its `siblings` resolved against the oracle, and, up to
/// [`ISLAND_OPS`] ops, against the island's fold, reading causality from `effective`: every
/// resolution a test runs checks itself, so every test that opens entries is a case of the
/// differential. A disagreement fails the test, or is counted under [`COUNT_ONLY`].
pub(in crate::membership_log) fn check(
    cands: &[CandidateOp],
    effective: &eval::Effective,
    register: &Register<'_>,
    siblings: &[usize],
) {
    let before = |a, b| effective.before(a, b);
    let (want, _) = resolve(cands, &before, usize::MAX);
    let want = want.expect("an unbounded budget never runs out");
    let fold = island(cands, &before);
    let got = resolved(register, siblings);
    let (checked, wrong) = CHECKED.get();
    let disagrees = got != want || fold.as_ref().is_some_and(|fold| *fold != want);
    CHECKED.set((checked + 1, wrong + usize::from(disagrees)));
    ISLAND_CHECKED.set(ISLAND_CHECKED.get() + usize::from(fold.is_some()));
    assert!(
        !disagrees || COUNT_ONLY.get(),
        "the register resolver, the one it replaced and the island's fold disagree: got {got:?}, \
         want {want:?}, the island's fold {fold:?}"
    );
}
