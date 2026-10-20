//! Chandy-Lamport consistent-cut lattice and causal-closure algebra over
//! multi-stream offset frontiers.
//!
//! Both [`astream-engine`](../astream_engine/index.html) (per-session envelope logs
//! indexed by `Partition(u64)`) and `aterm` fabric bridges (per-session or
//! per-subject broker streams indexed by `String` session/subject identifiers)
//! coordinate independent ordered streams connected by cross-stream causal edges
//! (`cause: (K, Offset) -> effect: (K, Offset)`).
//!
//! A [`CausalCut<K>`] assigns a frontier [`Offset`] to each stream `K`. By the
//! Chandy-Lamport distributed snapshot criterion (1985), a cut `C` is **causally
//! consistent** with respect to a set of cross-stream edges `E` iff every edge whose
//! effect lies inside `C` also has its cause inside `C`:
//!
//! ```text
//! ∀ ( (src, k) -> (dst, m) ) ∈ E :  m ≤ C[dst]  ⟹  k ≤ C[src]
//! ```
//!
//! Besides deciding consistency (`is_consistent`) and pinpointing the first
//! causal orphan (`first_violation`), [`CausalCut`] forms a bounded join-semilattice
//! (`join`, `meet`, `dominates`) and computes the minimal causal closure (`heal`)
//! that advances lagging source frontiers until every admitted effect's transitive
//! cause is included.

use crate::Offset;
use std::collections::BTreeMap;

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "cut_island.rs"]
mod cut_island;
#[cfg(not(clean_islands))]
pub use crate::twins::{causal_gate_frontier, heal_frontier_monotone, meet_frontier_monotone};
#[cfg(clean_islands)]
pub use cut_island::{causal_gate_frontier, heal_frontier_monotone, meet_frontier_monotone};

contract_fn! {
    /// Kernel-certified predicate deciding whether a stream frontier `upto` includes
    /// event offset `at_off` (`at_off <= upto`).
    #[must_use]
    pub const fn frontier_includes_raw(upto: u64, at_off: u64) -> bool
    clauses [ensures result == (at_off <= upto)]
    {
        at_off <= upto
    }
}

pub use frontier_includes_raw as frontier_includes;

/// 4-parameter Chandy-Lamport causal effect transition kernel (`causal_step_frontier_isl`),
/// composed directly from the 4-parameter Clean-Origin (`I5`) [`causal_gate_frontier`]
/// and [`heal_frontier_monotone`] functions.
#[must_use]
pub const fn causal_step_frontier(
    cause_cut: u64,
    cause_req: u64,
    eff_cur: u64,
    eff_cand: u64,
) -> u64 {
    if cause_cut < cause_req {
        eff_cur
    } else if eff_cur < eff_cand {
        eff_cand
    } else {
        eff_cur
    }
}

/// A directed causal dependency from `(cause.0, cause.1)` to `(effect.0, effect.1)`:
/// the event at `cause.1` on stream `cause.0` causally preceded the event at
/// `effect.1` on stream `effect.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CausalEdge<K> {
    /// The producing stream and the offset of the event that caused the effect.
    pub cause: (K, Offset),
    /// The receiving stream and the offset of the resulting event.
    pub effect: (K, Offset),
}

/// A witness that a [`CausalCut`] violates Chandy-Lamport consistency on `edge`:
/// the cut admits `edge.effect` (at `effect_frontier >= edge.effect.1`), but its
/// frontier on `edge.cause.0` is `cause_frontier` (`None` or `< edge.cause.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutViolation<K> {
    /// The cross-stream causal edge whose effect is admitted without its cause.
    pub edge: CausalEdge<K>,
    /// The cut's current frontier on `edge.cause.0` (`None` if the stream is absent).
    pub cause_frontier: Option<Offset>,
    /// The cut's current frontier on `edge.effect.0` (always `>= edge.effect.1`).
    pub effect_frontier: Offset,
}

/// A vector frontier mapping each stream identifier `K` to the highest [`Offset`]
/// included in the cut.
///
/// A stream absent from the map admits no offsets (`includes` returns `false` even
/// for `Offset::ZERO`), matching `astream-engine::fleet::Cut`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalCut<K: Ord> {
    frontiers: BTreeMap<K, Offset>,
}

impl<K: Ord> Default for CausalCut<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord> CausalCut<K> {
    /// An empty cut that names no streams (admits nothing).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            frontiers: BTreeMap::new(),
        }
    }

    /// Build a cut from `(stream, offset)` pairs. To match `astream-engine::fleet::Cut`'s
    /// first-entry-wins convention when a caller passes a slice containing duplicate
    /// stream keys, only the FIRST offset for each stream `K` is kept.
    #[must_use]
    pub fn from_first_wins<I: IntoIterator<Item = (K, Offset)>>(iter: I) -> Self {
        let mut frontiers = BTreeMap::new();
        for (k, off) in iter {
            frontiers.entry(k).or_insert(off);
        }
        Self { frontiers }
    }

    /// Set the frontier for `stream` to `upto`, replacing any previous frontier.
    pub fn set(&mut self, stream: K, upto: Offset) {
        self.frontiers.insert(stream, upto);
    }

    /// Monotonically advance `stream`'s frontier to at least `upto`. Returns `true`
    /// iff the frontier moved forward (or `stream` was newly inserted).
    pub fn advance(&mut self, stream: K, upto: Offset) -> bool {
        match self.frontiers.get_mut(&stream) {
            Some(cur) => {
                let next = Offset(heal_frontier_monotone(cur.0, upto.0));
                if next == *cur {
                    false
                } else {
                    *cur = next;
                    true
                }
            }
            None => {
                self.frontiers.insert(stream, upto);
                true
            }
        }
    }

    /// The frontier offset for `stream`, if the cut names it.
    #[must_use]
    pub fn offset_of(&self, stream: &K) -> Option<Offset> {
        self.frontiers.get(stream).copied()
    }

    /// Does the cut admit `at` on `stream` (`at <= offset_of(stream)`)?
    /// A stream not named in the cut admits nothing.
    #[must_use]
    pub fn includes(&self, stream: &K, at: Offset) -> bool {
        match self.offset_of(stream) {
            Some(upto) => frontier_includes_raw(upto.0, at.0),
            None => false,
        }
    }

    /// Number of streams named in the cut.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frontiers.len()
    }

    /// True if the cut names no streams.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frontiers.is_empty()
    }

    /// Iterate over `(stream, frontier)` pairs in ascending `K` order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, Offset)> {
        self.frontiers.iter().map(|(k, &v)| (k, v))
    }

    /// True iff `self` includes every frontier in `other` (`∀ (k, off) ∈ other,
    /// self.includes(k, off)`).
    #[must_use]
    pub fn dominates(&self, other: &Self) -> bool {
        for (k, &off) in &other.frontiers {
            if !self.includes(k, off) {
                return false;
            }
        }
        true
    }
}

impl<K: Ord + Clone> CausalCut<K> {
    /// Return the first [`CutViolation`] in `edges` where the cut admits `edge.effect`
    /// without admitting `edge.cause`, or `None` if the cut is Chandy-Lamport
    /// consistent.
    #[must_use]
    pub fn first_violation(&self, edges: &[CausalEdge<K>]) -> Option<CutViolation<K>> {
        for edge in edges {
            if let Some(effect_frontier) = self.offset_of(&edge.effect.0) {
                if frontier_includes_raw(effect_frontier.0, edge.effect.1 .0)
                    && !self.includes(&edge.cause.0, edge.cause.1)
                {
                    return Some(CutViolation {
                        edge: edge.clone(),
                        cause_frontier: self.offset_of(&edge.cause.0),
                        effect_frontier,
                    });
                }
            }
        }
        None
    }

    /// Chandy-Lamport consistency: for every edge whose effect the cut admits, it
    /// also admits the cause.
    #[must_use]
    pub fn is_consistent(&self, edges: &[CausalEdge<K>]) -> bool {
        self.first_violation(edges).is_none()
    }

    /// Advance this cut to its minimal Chandy-Lamport causal closure over `edges`:
    /// whenever an edge's effect is admitted by the cut (`effect.1 <= cut[effect.0]`)
    /// and its cause is not, advance `cut[cause.0]` to `edge.cause.1`.
    ///
    /// Iterates to a fixed point so transitive causal chains (`A -> B -> C`) are
    /// closed in one call. Because each step strictly raises the frontier of at
    /// least one edge's cause to that edge's `cause.1`, at most `edges.len()`
    /// rounds can make progress. Returns the total number of causal edge repairs
    /// applied (`0` if `self` was already consistent).
    pub fn heal(&mut self, edges: &[CausalEdge<K>]) -> usize {
        let mut repairs = 0usize;
        for _ in 0..edges.len() {
            let mut changed = false;
            for edge in edges {
                if self.includes(&edge.effect.0, edge.effect.1)
                    && !self.includes(&edge.cause.0, edge.cause.1)
                {
                    self.advance(edge.cause.0.clone(), edge.cause.1);
                    repairs = repairs.saturating_add(1);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        repairs
    }

    /// Least upper bound (pointwise maximum frontier) of two cuts.
    ///
    /// By construction, the join of any two consistent cuts is also consistent:
    /// if an effect is in `A ⊔ B`, it was in `A` or `B`, so its cause was in `A`
    /// or `B`, and therefore in `A ⊔ B`.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        let mut out = self.clone();
        for (k, &off) in &other.frontiers {
            out.advance(k.clone(), off);
        }
        out
    }

    /// Greatest lower bound (pointwise minimum frontier over streams present in both)
    /// of two cuts.
    ///
    /// By construction, the meet of any two consistent cuts is also consistent:
    /// if an effect is in `A ⊓ B`, it was in both `A` and `B`, so its cause was in
    /// both `A` and `B`, and therefore in `A ⊓ B`.
    #[must_use]
    pub fn meet(&self, other: &Self) -> Self {
        let mut frontiers = BTreeMap::new();
        for (k, &a) in &self.frontiers {
            if let Some(&b) = other.frontiers.get(k) {
                frontiers.insert(k.clone(), Offset(meet_frontier_monotone(a.0, b.0)));
            }
        }
        Self { frontiers }
    }

    /// Directly execute `Astream.Wire.TlaCausalCut.AdvanceEffectConsistent`:
    /// advance `effect_stream` to `effect_upto` via the 4-parameter Clean-origin
    /// [`causal_step_frontier`] / [`heal_frontier_monotone`] transition iff the causal
    /// parent `cause` (if any) is already admitted by this cut (`self.includes(cause_stream, cause_offset)`).
    /// Returns `true` iff the causal guard passed and the cut was updated/satisfied.
    pub fn advance_effect_if_causal(
        &mut self,
        effect_stream: K,
        effect_upto: Offset,
        cause: Option<(&K, Offset)>,
    ) -> bool {
        if let Some((cause_stream, cause_off)) = cause {
            let Some(cause_cut) = self.offset_of(cause_stream) else {
                return false;
            };
            if !frontier_includes(cause_cut.0, cause_off.0) {
                return false;
            }
            let eff_cur = self.offset_of(&effect_stream).map_or(0, |o| o.0);
            let next_eff = causal_step_frontier(cause_cut.0, cause_off.0, eff_cur, effect_upto.0);
            self.frontiers.insert(effect_stream, Offset(next_eff));
            return true;
        }
        self.advance(effect_stream, effect_upto);
        true
    }

    /// Directly execute `Astream.Wire.TlaCausalCut.HealCut` + `AdvanceEffectConsistent`
    /// in O(1) for a single delivered causal edge: if `cause` is present and not yet
    /// admitted by `self`, heal `cause.0` to `heal_frontier_monotone(cur, cause.1)` first,
    /// then advance `effect_stream` to `effect_upto`. Returns `true` if `cause` had
    /// to be healed.
    pub fn advance_and_heal_edge(
        &mut self,
        effect_stream: K,
        effect_upto: Offset,
        cause: Option<(K, Offset)>,
    ) -> bool {
        let mut healed_cause = false;
        if let Some((cause_stream, cause_off)) = cause {
            if !self.includes(&cause_stream, cause_off) {
                self.advance(cause_stream, cause_off);
                healed_cause = true;
            }
        }
        self.advance(effect_stream, effect_upto);
        healed_cause
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consistent_cut_admits_in_flight_and_rejects_orphaned_effect() {
        let edge = CausalEdge {
            cause: ("agent-a", Offset(5)),
            effect: ("agent-b", Offset(2)),
        };
        let mut cut = CausalCut::new();
        cut.set("agent-a", Offset(5));
        cut.set("agent-b", Offset(1));
        // In-flight (cause included, effect not yet included) is consistent.
        assert!(cut.is_consistent(&[edge]));

        // Including the effect while cause is at 4 violates consistency.
        cut.set("agent-a", Offset(4));
        cut.set("agent-b", Offset(2));
        assert!(!cut.is_consistent(&[edge]));
        let v = cut.first_violation(&[edge]).unwrap();
        assert_eq!(v.edge, edge);
        assert_eq!(v.cause_frontier, Some(Offset(4)));
        assert_eq!(v.effect_frontier, Offset(2));
    }

    #[test]
    fn heal_closes_transitive_causal_chains() {
        let edges = [
            CausalEdge {
                cause: ("planner", Offset(10)),
                effect: ("coder", Offset(4)),
            },
            CausalEdge {
                cause: ("coder", Offset(4)),
                effect: ("verifier", Offset(1)),
            },
        ];
        // Cut only names verifier at offset 1; heal must pull in coder@4 and planner@10.
        let mut cut = CausalCut::new();
        cut.set("verifier", Offset(1));
        assert!(!cut.is_consistent(&edges));

        let repairs = cut.heal(&edges);
        assert_eq!(repairs, 2);
        assert!(cut.is_consistent(&edges));
        assert_eq!(cut.offset_of(&"coder"), Some(Offset(4)));
        assert_eq!(cut.offset_of(&"planner"), Some(Offset(10)));
    }

    #[test]
    fn join_and_meet_preserve_consistency_and_dominance() {
        let edges = [
            CausalEdge {
                cause: (1u64, Offset(3)),
                effect: (2u64, Offset(7)),
            },
            CausalEdge {
                cause: (2u64, Offset(5)),
                effect: (3u64, Offset(2)),
            },
        ];
        let mut a = CausalCut::new();
        a.set(1, Offset(3));
        a.set(2, Offset(7));
        a.set(3, Offset(0));
        assert!(a.is_consistent(&edges));

        let mut b = CausalCut::new();
        b.set(1, Offset(1));
        b.set(2, Offset(5));
        b.set(3, Offset(2));
        assert!(b.is_consistent(&edges));

        let joined = a.join(&b);
        assert!(joined.is_consistent(&edges));
        assert!(joined.dominates(&a));
        assert!(joined.dominates(&b));
        assert_eq!(joined.offset_of(&1), Some(Offset(3)));
        assert_eq!(joined.offset_of(&2), Some(Offset(7)));
        assert_eq!(joined.offset_of(&3), Some(Offset(2)));

        let met = a.meet(&b);
        assert!(met.is_consistent(&edges));
        assert!(a.dominates(&met));
        assert!(b.dominates(&met));
        assert_eq!(met.offset_of(&1), Some(Offset(1)));
        assert_eq!(met.offset_of(&2), Some(Offset(5)));
        assert_eq!(met.offset_of(&3), Some(Offset(0)));
    }

    #[test]
    fn advance_effect_if_causal_and_heal_edge_enforce_tla_transitions() {
        let edge = CausalEdge {
            cause: ("src", Offset(10)),
            effect: ("dst", Offset(3)),
        };
        let mut cut = CausalCut::new();
        cut.set("src", Offset(9));
        assert!(!cut.advance_effect_if_causal("dst", Offset(3), Some((&"src", Offset(10)))));
        assert_eq!(cut.offset_of(&"dst"), None);

        // Healing the edge pulls src -> 10 and dst -> 3 in one atomic step.
        let healed = cut.advance_and_heal_edge("dst", Offset(3), Some(("src", Offset(10))));
        assert!(healed);
        assert!(cut.is_consistent(&[edge]));
        assert_eq!(cut.offset_of(&"src"), Some(Offset(10)));
        assert_eq!(cut.offset_of(&"dst"), Some(Offset(3)));

        // Subsequent causal advance succeeds without healing because src@10 is already admitted.
        assert!(cut.advance_effect_if_causal("dst", Offset(4), Some((&"src", Offset(10)))));
        assert_eq!(cut.offset_of(&"dst"), Some(Offset(4)));
    }
}
