// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The authorization pass the reader pass replaced, kept as its oracle: every record in order,
//! each read off the authorization and void of every record before it, with each Mac's records
//! found by its id rather than through the graph's actor table.

use super::*;

/// What the full pass reads of a graph: who the genesis binds, and each Mac's `Remove`s and the
/// `Admit`s others signed for it, in record order.
struct Full<'g, 'a> {
    graph: &'g Graph<'a>,
    founders: BTreeSet<MemberId>,
    removes: BTreeMap<MemberId, Vec<(usize, usize)>>,
    admits: BTreeMap<MemberId, Vec<usize>>,
}

impl<'g, 'a> Full<'g, 'a> {
    fn new(graph: &'g Graph<'a>) -> Self {
        let founders = graph.founders().map(|seat| seat.member()).collect();
        let mut removes: BTreeMap<MemberId, Vec<(usize, usize)>> = BTreeMap::new();
        let mut admits: BTreeMap<MemberId, Vec<usize>> = BTreeMap::new();
        for (i, record) in graph.records.iter().enumerate().skip(1) {
            match record.body() {
                Body::Remove { member, cutoff } => {
                    removes
                        .entry(*member)
                        .or_default()
                        .push((i, graph.at[cutoff]));
                }
                Body::Admit { entrant, .. } if record.actor() != &entrant.member() => {
                    admits.entry(entrant.member()).or_default().push(i);
                }
                _ => {}
            }
        }
        Self {
            graph,
            founders,
            removes,
            admits,
        }
    }

    /// What the candidates `quorums` void, every record by index: each one's member's records
    /// outside its cutoff's causal past.
    fn void(&self, candidates: &[Candidate], quorums: &[usize]) -> Vec<bool> {
        let graph = self.graph;
        let mut void = vec![false; graph.records.len()];
        for &q in quorums {
            let Candidate { member, cutoff, .. } = &candidates[q];
            for (i, is_void) in void.iter_mut().enumerate().skip(1) {
                if graph.signer(i) == member && !graph.upto(i, *cutoff) {
                    *is_void = true;
                }
            }
        }
        void
    }

    /// Whether each record is authorized when `void` (every record by index) holds the void
    /// ones: its signer is a member in its strict causal past.
    fn authorized(&self, void: &[bool]) -> Vec<bool> {
        let n = self.graph.records.len();
        let mut authorized = vec![false; n];
        authorized[0] = true;
        let mut effective = vec![0u32; n];
        let mut scope = 0;
        for i in 1..n {
            scope += 2;
            let signer = *self.graph.signer(i);
            authorized[i] =
                self.member_before(&signer, i, void, &authorized, &mut effective, scope);
        }
        authorized
    }

    fn removes_of(&self, mac: &MemberId) -> &[(usize, usize)] {
        self.removes.get(mac).map_or(&[], Vec::as_slice)
    }

    fn admits_of(&self, mac: &MemberId) -> &[usize] {
        self.admits.get(mac).map_or(&[], Vec::as_slice)
    }

    fn member_before(
        &self,
        mac: &MemberId,
        i: usize,
        void: &[bool],
        authorized: &[bool],
        effective: &mut [u32],
        scope: u32,
    ) -> bool {
        let graph = self.graph;
        let live = |r: usize| !void[r] && authorized[r];
        let removes = self.removes_of(mac);
        let removed = removes
            .iter()
            .take_while(|&&(r, _)| r < i)
            .any(|&(r, _)| live(r) && graph.before(r, i));
        if !removed && self.founders.contains(mac) {
            return true;
        }
        for &e in self.admits_of(mac) {
            if e >= i {
                break;
            }
            if !live(e) || !graph.before(e, i) {
                continue;
            }
            let after_every_remove = removes
                .iter()
                .take_while(|&&(r, _)| r < i)
                .filter(|&&(r, _)| live(r) && graph.before(r, i))
                .all(|&(r, _)| graph.before(r, e));
            if after_every_remove
                && self.effective_in_past(e, i, void, authorized, effective, scope)
            {
                return true;
            }
        }
        false
    }

    fn effective_in_past(
        &self,
        e: usize,
        i: usize,
        void: &[bool],
        authorized: &[bool],
        effective: &mut [u32],
        scope: u32,
    ) -> bool {
        if effective[e] == scope {
            return false;
        }
        if effective[e] == scope + 1 {
            return true;
        }
        let graph = self.graph;
        let live = |r: usize| !void[r] && authorized[r];
        let signer = *graph.signer(e);
        for &(r, cutoff) in self.removes_of(&signer) {
            if r >= i {
                break;
            }
            if !live(r) || !graph.before(r, i) || graph.upto(e, cutoff) {
                continue;
            }
            let restored = self
                .admits_of(&signer)
                .iter()
                .copied()
                .take_while(|&e2| e2 < e)
                .any(|e2| {
                    live(e2)
                        && graph.before(r, e2)
                        && graph.before(e2, e)
                        && self.effective_in_past(e2, i, void, authorized, effective, scope)
                });
            if !restored {
                effective[e] = scope;
                return false;
            }
        }
        effective[e] = scope + 1;
        true
    }
}

/// How many records the reader pass, extended to every record, authorizes otherwise than the full
/// pass, and how many the weighing's voids void otherwise than the full pass's: with nothing
/// void, under each candidate's effects alone, and under the quorums that take effect.
pub(in crate::membership_log) fn reader_pass_disagreements(records: &[Record]) -> usize {
    let indexed = Indexed::of(records);
    let graph = Graph::new(indexed.history(records));
    let full = Full::new(&graph);
    let mut memo = Memo::new();
    let mut scratch = graph.scratch();
    let weighing = graph.weighing(&mut memo, &mut scratch);
    let candidates = &weighing.candidates;
    let mut states = vec![Vec::new(), weighing.accepted.clone()];
    states.extend((0..candidates.len()).map(|q| vec![q]));
    let mut wrong = 0;
    for quorums in &states {
        let want_void = full.void(candidates, quorums);
        let void = graph.weighed_void(candidates, quorums);
        let auth = graph.authorize(&void, &mut memo, &mut scratch);
        let got = graph.authorized_records(&void, &auth, &mut scratch);
        let want = full.authorized(&want_void);
        let effects = graph.effects(candidates, quorums).void;
        for i in 0..records.len() {
            wrong += usize::from(got[i] != want[i]) + usize::from(effects[i] != want_void[i]);
            if graph.slot[i] != NOT_WEIGHED {
                wrong += usize::from(void[graph.slot[i]] != want_void[i]);
            }
        }
    }
    wrong
}
