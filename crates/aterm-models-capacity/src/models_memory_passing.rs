// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The memory passing law of build admission (CAP3, `aterm_capacity::admit::memory_turn`):
//! which claims go first past a waiter of memory that cannot start yet.

use aterm_spec::derive::*;

/// ONE WAITER OF MEMORY AND THE CLAIMS BEHIND IT, ON `Memory` UNITS. A head asks `Need` and, for
/// a need with a margin (`margin`, a release's slice budget or a VM's boot), one unit beside it;
/// such a need is never admitted alone. Memory no claim holds (`out`, a person's apps or a file
/// event daemon's growth) takes `Outside` or none, and can fall and rise again. A held claim's
/// tree holds the `Tree` it claims (`tree`, 0 once it ends), and the look reads it. Claims of one
/// unit behind the head (`small`, at most `Smalls` held) go first only by the passing rule. What
/// is available is `Memory - out - tree - small - running`, `running` the head's need once it
/// runs.
///
/// The rule (`SmallStarts`' guard), as `memory_turn` and `decide` apply it: a small claim starts
/// where it fits what is available and either no head waits, or the head starts now and it fits
/// beside the head's need (a head's margin is asked of the room, never reserved), or the head is
/// STUCK (it asks a margin and no set of claims' ends brings what it lacks: what is available
/// with every claim's end, `Memory - out - running`, is less than its need and margin), or the
/// head keeps its place and the claim fits what it SPARES. The ends here are the held claim's
/// `Tree` and each small claim's one, so the least set of them that covers what the head lacks
/// overshoots it by one unit exactly where the held claim alone is left and the head lacks one;
/// that unit is what it spares, and nowhere else does it spare any. A head of no margin is
/// admitted ALONE once no claim holds memory, so it keeps its place and spares nothing; while it
/// runs alone no other claim starts. `SmallRefused` is the look that refuses a small claim that
/// fits what is available, behind a waiting head: where the rule does not admit it.
///
/// The laws are written apart from the rule, on what each look did. `SmallStarts` marks a claim
/// that went first past a waiting head that keeps its place (one some set of ends starts, or one
/// of no margin) beyond what it spares (`delayed` for a head with a margin, `starved` for one of
/// none), and `SmallRefused` marks a claim held back behind a stuck head (`held_back`). Each
/// mutant dial changes the rule in both guards, as one change to `memory_turn` changes every
/// look: Buggy 1 lets a claim go first past every head with a margin that cannot start
/// (`FundedHeadKeepsItsTurn` falls); Buggy 2 is the rule before KI-1111-25, which held every
/// claim behind a stuck head (`StuckHeadHoldsNoClaimBack`); Buggy 3 treats a head of no margin
/// that cannot start as stuck (`AloneHeadKeepsItsPlace`).
///
/// The production projection (the conformance test in `aterm-capacity`'s `admit_stuck_tests`):
/// each unit is a GiB, the margin one GiB, the held claim and each small claim another process
/// whose tree the look reads, holding what it claims, and each look a `decide` on that ledger.
/// A held claim whose processes' use the look does not see, which keeps a head that its end may
/// start in its place sparing nothing, is not modelled: `admit_stuck_tests` pins it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn memory_passing_model() -> Model {
    aterm_spec::ty_model! {
        MemoryPassing {
            const Buggy = 0;
            const Memory = 10;
            const Need = 8;
            const Outside = 3;
            const Tree = 2;
            const Small = 1;
            const Smalls = 2;

            // Memory no claim holds: `Outside` taken, or none.
            var out = 3;
            // The held claim's tree: `Tree` while it is held, 0 once it ends.
            var tree = 2;
            // The head: 0 not yet queued, 1 waiting, 2 running, 3 ended.
            var head = 0;
            // Whether the head asks a unit beside its need (never admitted alone).
            var margin = 0;
            // What the running head holds: `Need`, or 0.
            var running = 0;
            // Whether the head was admitted ALONE.
            var alone = 0;
            // Small claims behind the head, held.
            var small = 0;
            // Whether a look has refused a small claim that fits behind a waiting head.
            var refused = 0;
            // A claim went first past a head with a margin some set of ends starts, beyond what
            // it spares.
            var delayed = 0;
            // A claim that fits was refused behind a head no claim's end can start.
            var held_back = 0;
            // A claim went first past a head of no margin that cannot start, beyond what it
            // spares.
            var starved = 0;

            action HeadQueuesWithMargin when (head == 0) {
                head = 1;
                margin = 1;
            }
            action HeadQueuesToRunAlone when (head == 0) {
                head = 1;
                margin = 0;
            }
            action HeadStarts when (head == 1
                && Need + margin <= Memory - out - tree - small) {
                head = 2;
                running = Need;
            }
            action HeadRunsAlone when (head == 1 && margin == 0 && tree == 0 && small == 0
                && Need > Memory - out) {
                head = 2;
                running = Need;
                alone = 1;
            }
            action HeadEnds when (head == 2) {
                head = 3;
                running = 0;
                alone = 0;
            }
            action HolderEnds when (tree == Tree) {
                tree = 0;
            }
            action OutsideFrees when (out == Outside) {
                out = 0;
            }
            action OutsideTakes when (out == 0) {
                out = Outside;
            }
            action SmallStarts when (alone == 0 && Smalls > small
                && Small <= Memory - out - tree - small - running
                && (head == 0 || head > 1
                    || (Need + margin <= Memory - out - tree - small
                        && Need + Small <= Memory - out - tree - small)
                    || ((Buggy <= 1 || Buggy > 2) && margin == 1 && Need + margin > Memory - out)
                    || (Need + margin <= Memory - out && tree == Tree && small == 0
                        && Memory - out - tree + 1 == Need + margin)
                    || (Buggy == 1 && head == 1 && margin == 1)
                    || (Buggy == 3 && head == 1 && margin == 0 && Need > Memory - out))) {
                small = small + 1;
                delayed = if (head == 1 && margin == 1 && Need + margin <= Memory - out
                    && (Need + margin > Memory - out - tree - small
                        || Need + Small > Memory - out - tree - small)
                    && (Tree > tree || small > 0 || Memory - out - tree + 1 > Need + margin
                        || Need + margin > Memory - out - tree + 1)) {
                    1
                } else {
                    delayed
                };
                starved = if (head == 1 && margin == 0
                    && (Need > Memory - out - tree - small
                        || Need + Small > Memory - out - tree - small)
                    && (Need > Memory - out || Tree > tree || small > 0
                        || Memory - out - tree + 1 > Need || Need > Memory - out - tree + 1)) {
                    1
                } else {
                    starved
                };
            }
            action SmallRefused when (head == 1 && Smalls > small
                && Small <= Memory - out - tree - small
                && (Need + margin > Memory - out - tree - small
                    || Need + Small > Memory - out - tree - small)
                && (Buggy == 2 || margin == 0 || Need + margin <= Memory - out)
                && (Need + margin > Memory - out || Tree > tree || small > 0
                    || Memory - out - tree + 1 > Need + margin
                    || Need + margin > Memory - out - tree + 1)
                && (Buggy == 0 || Buggy > 1 || margin == 0)
                && (Buggy <= 2 || Buggy > 3 || margin == 1 || Need <= Memory - out)) {
                refused = 1;
                held_back = if (margin == 1 && Need + margin > Memory - out) {
                    1
                } else {
                    held_back
                };
            }
            action SmallEnds when (small > 0) {
                small = small - 1;
            }

            invariant FundedHeadKeepsItsTurn: delayed == 0;
            invariant StuckHeadHoldsNoClaimBack: held_back == 0;
            invariant AloneHeadKeepsItsPlace: starved == 0;
            bound PassingIsFinite: out <= Outside && tree <= Tree && head <= 3 && margin <= 1
                && running <= Need && alone <= 1 && small <= Smalls && refused <= 1
                && delayed <= 1 && held_back <= 1 && starved <= 1;
        }
    }
}
