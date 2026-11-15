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
/// PARENTS: the held claim may be the head's parent (`kin`, set before the head queues: its
/// holder is an ancestor of the head's process, so the head is part of the work it holds). Then,
/// once the head has queued, it ends only after the head has run (it may end before the head
/// queues), its end is none the head's turn can wait for, and the head's own look leaves it out:
/// that look admits a head of no margin ALONE beside it once no small claim is held. A head with
/// a margin that only the parent's end would fund is stuck.
///
/// OUTER: a head with a margin under its parent may queue behind another process's waiter
/// (`outer`, set as the head queues), a budget of `Outer` with a unit beside it, which no look
/// sees the use of and which waits as long as the head does. The head's own look, inside its
/// parent's claim, leaves that waiter out, so its turn counts neither its reservation nor its
/// end, and it is stuck; the claims behind them are held to what the outer waiter spares, which
/// is one unit exactly where the held claim alone is left and the outer waiter lacks one.
///
/// The laws are written apart from the rule, on what each look did. `SmallStarts` marks a claim
/// that went first past a waiting head that keeps its place (one some set of ends it can wait
/// for starts, or one of no margin) beyond what it spares (`delayed` for a head with a margin,
/// `starved` for one of none), and `SmallRefused` marks a claim held back behind a stuck head
/// (`held_back`). Each mutant dial changes the rule in both guards, as one change to
/// `memory_turn` changes every look: Buggy 1 lets a claim go first past every head with a margin
/// that cannot start (`FundedHeadKeepsItsTurn` falls); Buggy 2 is the rule before KI-1111-25,
/// which held every claim behind a stuck head (`StuckHeadHoldsNoClaimBack`); Buggy 3 treats a
/// head of no margin that cannot start as stuck (`AloneHeadKeepsItsPlace`); Buggy 4 is the rule
/// before KI-1113-21, which counted the parent's end among those a head's turn waits for and
/// held a claim behind a head only that end would fund (`StuckHeadHoldsNoClaimBack`); Buggy 5 is
/// the rule before KI-1113-342, which stood an inside head beside the outer waiter, counting its
/// reservation and its end with as much again unseen, and held a claim behind a stuck head that
/// the outer waiter spared (`StuckHeadHoldsNoClaimBack`).
///
/// The production projection (the conformance test in `aterm-capacity`'s `admit_stuck_tests`):
/// each unit is a GiB, the margin one GiB, the held claim and each small claim another process
/// whose tree the look reads, holding what it claims, the held claim's process the head's
/// process's parent in the look's process sweep where `kin` is set, the outer waiter a
/// cohort-owned claim of another process asked first where `outer` is, and each look a `decide`
/// on the claims its own look counts. A held claim whose processes' use the look does not see,
/// which keeps a head that its end may start in its place sparing nothing, is not modelled, nor
/// is a parent whose tree holds less than it claims, whose reservation the head's own look and
/// its turn in every other look leave out, nor a cohort-owned head, which has no kin, since its
/// own look counts its parent's claim, nor a sure lead an inside head counts: `admit_stuck_tests`
/// pins all four, a kin's unseen use included.
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
            const Outer = 8;

            // Memory no claim holds: `Outside` taken, or none.
            var out = 3;
            // The held claim's tree: `Tree` while it is held, 0 once it ends.
            var tree = 2;
            // Whether the held claim is the head's PARENT (its holder an ancestor of the head's
            // process), which, once the head has queued, ends only after the head has run.
            var kin = 0;
            // Whether another process's waiter asked ahead of an inside head (OUTER), waiting.
            var outer = 0;
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

            action HolderSpawnsHead when (head == 0 && tree == Tree && kin == 0) {
                kin = 1;
            }
            action HeadQueuesWithMargin when (head == 0) {
                head = 1;
                margin = 1;
            }
            action HeadQueuesToRunAlone when (head == 0) {
                head = 1;
                margin = 0;
            }
            action HeadQueuesInsideBehindOuter when (head == 0 && kin == 1 && tree == Tree) {
                head = 1;
                margin = 1;
                outer = 1;
            }
            action HeadStarts when (head == 1
                && Need + margin <= Memory - out - tree - small) {
                head = 2;
                running = Need;
            }
            action HeadRunsAlone when (head == 1 && margin == 0 && (tree == 0 || kin == 1)
                && small == 0 && Need > Memory - out - tree) {
                head = 2;
                running = Need;
                alone = 1;
            }
            action HeadEnds when (head == 2) {
                head = 3;
                running = 0;
                alone = 0;
            }
            action HolderEnds when (tree == Tree && (kin == 0 || head == 0 || head == 3)) {
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
                && (outer == 0 || Outer + 1 > Memory - out
                    || (small == 0 && Memory - out == Outer + 2))
                && (head == 0 || head > 1
                    || (Need + margin <= Memory - out - tree - small
                        && Need + Small <= Memory - out - tree - small)
                    || ((Buggy <= 1 || Buggy > 2) && margin == 1
                        && (Buggy <= 4 || Buggy > 5 || outer == 0 || Outer + 1 > Memory - out)
                        && (Need + margin > Memory - out
                            || ((Buggy <= 3 || Buggy > 4) && kin == 1
                                && Need + margin > Memory - out - tree)))
                    || ((kin == 0 || Buggy == 4) && Need + margin <= Memory - out
                        && tree == Tree && small == 0
                        && Memory - out - tree + 1 == Need + margin)
                    || (Buggy == 1 && head == 1 && margin == 1)
                    || (Buggy == 3 && head == 1 && margin == 0 && Need > Memory - out))) {
                small = small + 1;
                delayed = if (head == 1 && margin == 1 && Need + margin <= Memory - out
                    && (kin == 0 || Need + margin <= Memory - out - tree)
                    && (Need + margin > Memory - out - tree - small
                        || Need + Small > Memory - out - tree - small)
                    && (kin == 1 || Tree > tree || small > 0
                        || Memory - out - tree + 1 > Need + margin
                        || Need + margin > Memory - out - tree + 1)) {
                    1
                } else {
                    delayed
                };
                starved = if (head == 1 && margin == 0
                    && (Need > Memory - out - tree - small
                        || Need + Small > Memory - out - tree - small)
                    && (kin == 1 || Need > Memory - out || Tree > tree || small > 0
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
                && (Buggy == 2 || margin == 0
                    || (Need + margin <= Memory - out
                        && (kin == 0 || Buggy == 4 || Need + margin <= Memory - out - tree))
                    || (outer == 1 && Outer + 1 <= Memory - out
                        && (small > 0 || Memory - out > Outer + 2 || Outer + 2 > Memory - out))
                    || (Buggy == 5 && outer == 1 && Outer + 1 <= Memory - out))
                && ((kin == 1 && (Buggy <= 3 || Buggy > 4))
                    || Need + margin > Memory - out || Tree > tree || small > 0
                    || Memory - out - tree + 1 > Need + margin
                    || Need + margin > Memory - out - tree + 1)
                && (Buggy == 0 || Buggy > 1 || margin == 0
                    || (outer == 1 && Outer + 1 <= Memory - out
                        && (small > 0 || Memory - out > Outer + 2 || Outer + 2 > Memory - out)))
                && (Buggy <= 2 || Buggy > 3 || margin == 1 || Need <= Memory - out)) {
                refused = 1;
                held_back = if (margin == 1 && (Need + margin > Memory - out
                    || (kin == 1 && Need + margin > Memory - out - tree))
                    && (outer == 0 || Outer + 1 > Memory - out
                        || (small == 0 && Memory - out == Outer + 2))) {
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
            bound PassingIsFinite: out <= Outside && tree <= Tree && kin <= 1 && outer <= 1
                && head <= 3
                && margin <= 1
                && running <= Need && alone <= 1 && small <= Smalls && refused <= 1
                && delayed <= 1 && held_back <= 1 && starved <= 1;
        }
    }
}
