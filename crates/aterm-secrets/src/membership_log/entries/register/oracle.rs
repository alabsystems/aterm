// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The register resolver [`super::Register`] replaced, kept only as the tests' oracle: for every
//! op, a scan of every earlier op with a test against each newest one found so far, and a test
//! of each pair among the newest. It counts its causal tests, and gives up once they pass a
//! budget, so a test can show it breaks a bound without running out the cubic count.
//!
//! Every resolution the tests run checks itself against it ([`check`]).

use super::super::{CandidateOp, DecodedOp, same_op};
use super::Register;
use crate::membership_log::eval;
use std::cell::Cell;

/// The budget ran out.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::membership_log) struct Exhausted;

/// What the replaced resolver read off a register: each op's carry and rep, and the siblings.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::membership_log) struct Resolved {
    pub(in crate::membership_log) carries: Vec<bool>,
    pub(in crate::membership_log) reps: Vec<usize>,
    pub(in crate::membership_log) siblings: Vec<usize>,
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
        Ok(Resolved {
            siblings: old.siblings()?,
            carries: old.carries.clone(),
            reps: old.reps.clone(),
        })
    });
    (resolved, old.tests.get())
}

/// What `register` and its `siblings` resolved, as [`resolve`] gives it.
pub(in crate::membership_log) fn resolved(register: &Register<'_>, siblings: &[usize]) -> Resolved {
    Resolved {
        carries: register.carries.clone(),
        reps: register.reps.clone(),
        siblings: siblings.to_vec(),
    }
}

thread_local! {
    /// Registers checked against the oracle on this thread, and how many disagreed.
    pub(in crate::membership_log) static CHECKED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// Count a disagreement rather than fail on it: a negative control's mutant disagrees.
    pub(in crate::membership_log) static COUNT_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Check what `register` and its `siblings` resolved against the oracle, reading causality from
/// `effective`: every resolution a test runs checks itself, so every test that opens entries
/// is a case of the differential. A disagreement fails the test, or is counted under
/// [`COUNT_ONLY`].
pub(in crate::membership_log) fn check(
    cands: &[CandidateOp],
    effective: &eval::Effective,
    register: &Register<'_>,
    siblings: &[usize],
) {
    let (want, _) = resolve(cands, &|a, b| effective.before(a, b), usize::MAX);
    let want = want.expect("an unbounded budget never runs out");
    let got = resolved(register, siblings);
    let (checked, wrong) = CHECKED.get();
    let disagrees = got != want;
    CHECKED.set((checked + 1, wrong + usize::from(disagrees)));
    assert!(
        !disagrees || COUNT_ONLY.get(),
        "the register resolver disagrees with the one it replaced: got {got:?}, want {want:?}"
    );
}
